import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App from "./App";
import {
  clearMocks,
  emitStatus,
  localDate,
  mockCommand,
  type AppInfo,
  type AppUsage,
  type DayReport,
  type Episode,
  type Status,
} from "./lib/ipc";

const DATA_DIR = String.raw`C:\Users\you\AppData\Roaming\openhistory-win`;
const MINUTE = 60_000;

function status(overrides: Partial<Status> = {}): Status {
  return {
    running: true,
    eventsToday: 2,
    lastEventAt: "2026-08-21T14:05:00.000Z",
    dataDir: DATA_DIR,
    ...overrides,
  };
}

function episode(overrides: Partial<Episode> = {}): Episode {
  return {
    id: "2026-08-21#1",
    date: "2026-08-21",
    app: "Visual Studio Code",
    title: "collector.rs - openhistory-win",
    start: "2026-08-21T14:05:00.000Z",
    end: "2026-08-21T14:35:00.000Z",
    durationMs: 30 * MINUTE,
    activeMs: 30 * MINUTE,
    eventCount: 4,
    isPrivate: false,
    ...overrides,
  };
}

/** Build the rollup the backend would have derived from these episodes. */
function report(episodes: Episode[] = [episode()]): DayReport {
  const totals = new Map<string, AppUsage>();
  for (const one of episodes) {
    const running = totals.get(one.app) ?? { app: one.app, activeMs: 0, episodes: 0 };
    running.activeMs += one.activeMs;
    running.episodes += 1;
    totals.set(one.app, running);
  }

  return {
    date: localDate(),
    episodes,
    rollup: {
      date: localDate(),
      activeMs: episodes.reduce((sum, one) => sum + one.activeMs, 0),
      episodes: episodes.length,
      apps: [...totals.values()].sort((a, b) => b.activeMs - a.activeMs),
      hours: [],
      privateEpisodes: episodes.filter((one) => one.isPrivate).length,
    },
  };
}

/** Register a working backend. Individual tests override what they care about. */
function backend(day: DayReport = report(), initial: Status = status()) {
  let current = initial;
  mockCommand(
    "app_info",
    (): AppInfo => ({ name: "OpenHistory", version: "0.1.0", phase: 3, dataDir: DATA_DIR }),
  );
  mockCommand("get_status", () => current);
  mockCommand("day_report", () => day);
  mockCommand("stop_collector", () => {
    current = { ...current, running: false };
    return current;
  });
  mockCommand("start_collector", () => {
    current = { ...current, running: true };
    return current;
  });
}

afterEach(() => {
  clearMocks();
  vi.restoreAllMocks();
});

describe("App shell", () => {
  it("shows the app version once IPC resolves", async () => {
    backend();
    mockCommand(
      "app_info",
      (): AppInfo => ({ name: "OpenHistory", version: "9.9.9", phase: 3, dataDir: DATA_DIR }),
    );
    render(<App />);

    expect(await screen.findByText(/v9\.9\.9/)).toBeInTheDocument();
  });

  it("surfaces an IPC failure instead of rendering a blank state", async () => {
    backend();
    mockCommand("app_info", () => {
      throw new Error("collector offline");
    });
    render(<App />);

    expect(await screen.findByRole("alert")).toHaveTextContent("collector offline");
  });

  it("says where history is kept", async () => {
    backend();
    render(<App />);

    expect(await screen.findByText(new RegExp(DATA_DIR.replace(/\\/g, "\\\\")))).toBeInTheDocument();
  });
});

describe("Recording status", () => {
  it("reports recording and the day's count", async () => {
    backend(report([]), status({ eventsToday: 7 }));
    render(<App />);

    expect(await screen.findByText("Recording")).toBeInTheDocument();
    expect(screen.getByText(/7 events today/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Pause" })).toHaveAttribute("aria-pressed", "true");
  });

  it("reports being paused", async () => {
    backend(report([]), status({ running: false }));
    render(<App />);

    expect(await screen.findByText("Paused")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Resume" })).toHaveAttribute("aria-pressed", "false");
  });

  it("pauses and resumes through the backend", async () => {
    backend();
    render(<App />);

    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Pause" }));

    expect(await screen.findByText("Paused")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Resume" }));
    expect(await screen.findByText("Recording")).toBeInTheDocument();
  });

  it("follows status pushed from the backend", async () => {
    backend();
    render(<App />);
    await screen.findByText("Recording");

    emitStatus(status({ running: false, eventsToday: 41 }));

    expect(await screen.findByText("Paused")).toBeInTheDocument();
    expect(screen.getByText(/41 events today/)).toBeInTheDocument();
  });

  it("reports a failed toggle rather than lying about the state", async () => {
    backend();
    mockCommand("stop_collector", () => {
      throw new Error("the collector would not stop");
    });
    render(<App />);

    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Pause" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("the collector would not stop");
    expect(screen.getByText("Recording")).toBeInTheDocument();
  });
});

describe("Today", () => {
  it("asks for today's local date", async () => {
    backend();
    const seen: unknown[] = [];
    mockCommand("day_report", (args) => {
      seen.push(args?.date);
      return report();
    });
    render(<App />);

    await waitFor(() => expect(seen).toContain(localDate()));
  });

  it("lists the day's episodes newest first", async () => {
    backend(
      report([
        episode({ id: "a", start: "2026-08-21T09:00:00.000Z", title: "earlier" }),
        episode({ id: "b", start: "2026-08-21T17:00:00.000Z", title: "later" }),
      ]),
    );
    render(<App />);

    const items = await screen.findAllByRole("listitem");
    expect(items).toHaveLength(2);
    expect(items[0]).toHaveTextContent("later");
    expect(items[1]).toHaveTextContent("earlier");
  });

  it("measures each episode and the day as a whole", async () => {
    backend(
      report([
        episode({ id: "a", app: "Visual Studio Code", activeMs: 90 * MINUTE }),
        episode({ id: "b", app: "Google Chrome", activeMs: 30 * MINUTE }),
      ]),
    );
    render(<App />);

    expect(await screen.findByText(/2h active/)).toBeInTheDocument();
    expect(screen.getByText(/2 episodes/)).toBeInTheDocument();
    expect(screen.getByText(/mostly Visual Studio Code/)).toBeInTheDocument();
    expect(screen.getByText("1h 30m")).toBeInTheDocument();
    expect(screen.getByText("30m")).toBeInTheDocument();
  });

  it("names a private session without showing anything about it", async () => {
    const secret = episode({ id: "p1", app: "Google Chrome", isPrivate: true });
    delete secret.title;
    backend(report([secret]));
    render(<App />);

    expect(await screen.findByText(/Private browsing/)).toBeInTheDocument();
    expect(screen.getByText("Google Chrome")).toBeInTheDocument();
    expect(screen.getByText(/1 private session/)).toBeInTheDocument();
  });

  it("explains an empty day rather than showing a bare list", async () => {
    backend(report([]));
    render(<App />);

    expect(await screen.findByText(/Nothing recorded yet today/)).toBeInTheDocument();
    expect(screen.queryAllByRole("listitem")).toHaveLength(0);
  });

  it("reprocesses the day after the backend records something", async () => {
    let day = report([episode({ id: "a", title: "first" })]);
    backend(day);
    mockCommand("day_report", () => day);
    render(<App />);
    await screen.findByText("first");

    day = report([episode({ id: "a", title: "first" }), episode({ id: "b", title: "second" })]);
    emitStatus(status({ eventsToday: 9 }));

    // The refresh trails the last event, so this deliberately outlasts the debounce.
    expect(await screen.findByText("second", undefined, { timeout: 3000 })).toBeInTheDocument();
  });
});
