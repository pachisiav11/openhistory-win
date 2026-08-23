import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App from "./App";
import { clearMocks, emitStatus, mockCommand, type AppInfo } from "./lib/ipc";
import { DATA_DIR, MINUTE, backend, episode, hit, report, status } from "./test/fixtures";

afterEach(() => {
  clearMocks();
  vi.restoreAllMocks();
});

describe("App shell", () => {
  it("shows the app version once IPC resolves", async () => {
    backend();
    mockCommand(
      "app_info",
      (): AppInfo => ({ name: "OpenHistory", version: "9.9.9", phase: 6, dataDir: DATA_DIR }),
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

    expect(
      await screen.findByText(new RegExp(DATA_DIR.replace(/\\/g, "\\\\"))),
    ).toBeInTheDocument();
  });

  it("opens on the timeline and moves between views", async () => {
    backend();
    render(<App />);

    expect(await screen.findByRole("button", { name: "Timeline" })).toHaveAttribute(
      "aria-current",
      "page",
    );

    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Search" }));
    expect(await screen.findByRole("searchbox", { name: "Search history" })).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Settings" }));
    expect(await screen.findByRole("region", { name: "Recording" })).toBeInTheDocument();
  });

  it("carries a search result through to the hour of its day", async () => {
    const start = new Date(2026, 7, 19, 10, 30).toISOString();
    backend({
      day: report(
        [episode({ start })],
        [
          { hour: 9, activeMs: 45 * MINUTE, apps: [], episodeIds: ["a"] },
          { hour: 10, activeMs: 15 * MINUTE, apps: [], episodeIds: ["b"] },
        ],
      ),
    });
    mockCommand("search_history", () => [hit({ date: "2026-08-19", start })]);
    render(<App />);

    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Search" }));
    await user.type(screen.getByRole("searchbox", { name: "Search history" }), "code");
    await user.click(await screen.findByRole("button", { name: /Visual Studio Code/ }));

    expect(await screen.findByRole("region", { name: "Day view" })).toBeInTheDocument();
    expect(screen.getByText("2026-08-19")).toBeInTheDocument();
    await waitFor(() =>
      expect(screen.getByText("10:00").closest("li")).toHaveAttribute("aria-current", "location"),
    );
  });
});

describe("Recording status", () => {
  it("reports recording and the day's count", async () => {
    backend({ day: report([]), status: status({ eventsToday: 7 }) });
    render(<App />);

    expect(await screen.findByText("Recording")).toBeInTheDocument();
    expect(screen.getByText(/7 events today/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Pause" })).toHaveAttribute("aria-pressed", "true");
  });

  it("reports being paused", async () => {
    backend({ day: report([]), status: status({ running: false }) });
    render(<App />);

    expect(await screen.findByText("Paused")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Resume" })).toHaveAttribute(
      "aria-pressed",
      "false",
    );
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
