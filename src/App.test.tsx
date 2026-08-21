import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App from "./App";
import {
  clearMocks,
  emitStatus,
  localDate,
  mockCommand,
  type ActivityEvent,
  type AppInfo,
  type Status,
} from "./lib/ipc";

const DATA_DIR = String.raw`C:\Users\you\AppData\Roaming\openhistory-win`;

function status(overrides: Partial<Status> = {}): Status {
  return {
    running: true,
    eventsToday: 2,
    lastEventAt: "2026-08-21T14:05:00.000Z",
    dataDir: DATA_DIR,
    ...overrides,
  };
}

function event(overrides: Partial<ActivityEvent> = {}): ActivityEvent {
  return {
    version: 1,
    id: "e1",
    timestamp: "2026-08-21T14:05:00.000Z",
    kind: "applicationActivated",
    application: { name: "Visual Studio Code", path: String.raw`C:\Code.exe`, pid: 1 },
    windowTitle: "collector.rs - openhistory-win",
    ...overrides,
  };
}

/** Register a working backend. Individual tests override what they care about. */
function backend(events: ActivityEvent[] = [event()], initial: Status = status()) {
  let current = initial;
  mockCommand(
    "app_info",
    (): AppInfo => ({ name: "OpenHistory", version: "0.1.0", phase: 2, dataDir: DATA_DIR }),
  );
  mockCommand("get_status", () => current);
  mockCommand("read_day", () => events);
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
      (): AppInfo => ({ name: "OpenHistory", version: "9.9.9", phase: 2, dataDir: DATA_DIR }),
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
    backend([], status({ eventsToday: 7 }));
    render(<App />);

    expect(await screen.findByText("Recording")).toBeInTheDocument();
    expect(screen.getByText(/7 events today/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Pause" })).toHaveAttribute("aria-pressed", "true");
  });

  it("reports being paused", async () => {
    backend([], status({ running: false }));
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
    mockCommand("read_day", (args) => {
      seen.push(args?.date);
      return [event()];
    });
    render(<App />);

    await waitFor(() => expect(seen).toContain(localDate()));
  });

  it("lists the day's events newest first", async () => {
    backend([
      event({ id: "a", timestamp: "2026-08-21T09:00:00.000Z", windowTitle: "earlier" }),
      event({ id: "b", timestamp: "2026-08-21T17:00:00.000Z", windowTitle: "later" }),
    ]);
    render(<App />);

    const items = await screen.findAllByRole("listitem");
    expect(items).toHaveLength(2);
    expect(items[0]).toHaveTextContent("later");
    expect(items[1]).toHaveTextContent("earlier");
  });

  it("shows a URL when one was recorded", async () => {
    backend([
      event({
        kind: "urlChanged",
        application: { name: "Google Chrome", path: String.raw`C:\chrome.exe`, pid: 2 },
        windowTitle: "Win32 accessibility - Google Chrome",
        browser: { url: "https://learn.microsoft.com/windows/win32/", isPrivate: false },
      }),
    ]);
    render(<App />);

    expect(
      await screen.findByText("https://learn.microsoft.com/windows/win32/"),
    ).toBeInTheDocument();
  });

  it("names a private session without showing anything about it", async () => {
    backend([
      {
        version: 1,
        id: "p1",
        timestamp: "2026-08-21T14:05:00.000Z",
        kind: "privacyBoundary",
        application: { name: "Google Chrome", path: String.raw`C:\chrome.exe`, pid: 2 },
        browser: { isPrivate: true },
      },
    ]);
    render(<App />);

    expect(await screen.findByText(/Private browsing/)).toBeInTheDocument();
    expect(screen.getByText("Nothing was recorded")).toBeInTheDocument();
  });

  it("explains an empty day rather than showing a bare list", async () => {
    backend([]);
    render(<App />);

    expect(await screen.findByText(/Nothing recorded yet today/)).toBeInTheDocument();
    expect(screen.queryAllByRole("listitem")).toHaveLength(0);
  });
});
