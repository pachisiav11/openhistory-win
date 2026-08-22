import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import DayView from "./DayView";
import { clearMocks, localDate, mockCommand, type RunReport } from "../lib/ipc";
import { MINUTE, backend, episode, report, summary } from "../test/fixtures";

afterEach(() => {
  clearMocks();
  vi.restoreAllMocks();
});

const READY = { provider: "anthropic", ready: true, model: "claude-haiku-4-5" };

function hours() {
  return [
    { hour: 9, activeMs: 45 * MINUTE, apps: [], episodeIds: ["a"] },
    { hour: 10, activeMs: 15 * MINUTE, apps: [], episodeIds: ["b"] },
  ];
}

describe("Day view", () => {
  it("shows the hours and where the time went", async () => {
    backend();
    mockCommand("day_report", () =>
      report(
        [
          episode({ id: "a", app: "Visual Studio Code", activeMs: 45 * MINUTE }),
          episode({ id: "b", app: "Slack", activeMs: 15 * MINUTE }),
        ],
        hours(),
      ),
    );
    render(<DayView date="2026-08-21" onChangeDate={() => {}} revision={0} />);

    expect(await screen.findByText("09:00")).toBeInTheDocument();
    expect(screen.getByText("10:00")).toBeInTheDocument();
    expect(screen.getByText("Visual Studio Code")).toBeInTheDocument();
    expect(screen.getByText("Slack")).toBeInTheDocument();
  });

  it("shows a written summary", async () => {
    backend();
    mockCommand("day_summary", () =>
      summary({ date: "2026-08-21", daily: "A morning of Rust and a short read." }),
    );
    render(<DayView date="2026-08-21" onChangeDate={() => {}} revision={0} />);

    expect(await screen.findByText("A morning of Rust and a short read.")).toBeInTheDocument();
  });

  it("says why it cannot write one, rather than hiding the button", async () => {
    backend({
      readiness: {
        provider: "anthropic",
        ready: false,
        blockedBy: "Cloud summaries need your agreement before anything is sent.",
      },
    });
    render(<DayView date="2026-08-21" onChangeDate={() => {}} revision={0} />);

    expect(await screen.findByText(/need your agreement/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Write summary" })).toBeDisabled();
  });

  it("writes a summary and shows it", async () => {
    let written = false;
    backend({ readiness: READY });
    mockCommand("day_report", () => report([episode()], hours()));
    mockCommand("day_summary", () =>
      written ? summary({ date: "2026-08-21", daily: "A good day's work." }) : summary(),
    );
    mockCommand("summarize_day", (): RunReport => {
      written = true;
      return {
        date: "2026-08-21",
        hoursWritten: [9, 10],
        hoursSkipped: [],
        hoursTooQuiet: [],
        dailyWritten: true,
      };
    });
    render(<DayView date="2026-08-21" onChangeDate={() => {}} revision={0} />);

    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Write summary" }));

    expect(await screen.findByText("A good day's work.")).toBeInTheDocument();
    expect(screen.getByText(/Wrote 2 hours and the day/)).toBeInTheDocument();
  });

  it("offers to write only the new hours once a day has a summary", async () => {
    backend({ readiness: READY });
    mockCommand("day_report", () => report([episode()], hours()));
    mockCommand("day_summary", () => summary({ date: "2026-08-21", daily: "A good day's work." }));
    render(<DayView date="2026-08-21" onChangeDate={() => {}} revision={0} />);

    expect(await screen.findByRole("button", { name: "Write new hours" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Write summary" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Rewrite" })).toBeInTheDocument();
  });

  it("reports a provider failure instead of claiming a summary was written", async () => {
    backend({ readiness: READY });
    mockCommand(
      "summarize_day",
      (): RunReport => ({
        date: "2026-08-21",
        hoursWritten: [],
        hoursSkipped: [],
        hoursTooQuiet: [],
        dailyWritten: false,
        failure: "the provider refused the request",
      }),
    );
    render(<DayView date="2026-08-21" onChangeDate={() => {}} revision={0} />);

    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Write summary" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("the provider refused the request");
  });

  it("writes one hour on its own", async () => {
    backend({ readiness: READY });
    mockCommand("day_report", () => report([episode()], hours()));
    const asked: number[] = [];
    mockCommand("summarize_hour", (args) => {
      asked.push(Number(args?.hour));
      return { hour: Number(args?.hour), text: "An hour of work.", activeMs: 0, generatedAt: "", provider: "anthropic", model: "claude-haiku-4-5" };
    });
    render(<DayView date="2026-08-21" onChangeDate={() => {}} revision={0} />);

    const user = userEvent.setup();
    const buttons = await screen.findAllByRole("button", { name: "Summarize this hour" });
    await user.click(buttons[0]!);

    expect(asked).toEqual([9]);
  });

  it("moves between days and back to today", async () => {
    backend();
    const dates: string[] = [];
    render(<DayView date="2026-08-21" onChangeDate={(date) => dates.push(date)} revision={0} />);

    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "‹ Previous" }));
    await user.click(screen.getByRole("button", { name: "Next ›" }));
    await user.click(screen.getByRole("button", { name: "Today" }));

    expect(dates).toEqual(["2026-08-20", "2026-08-22", localDate()]);
  });

  it("will not walk into the future", async () => {
    backend();
    render(<DayView date={localDate()} onChangeDate={() => {}} revision={0} />);

    expect(await screen.findByRole("button", { name: "Next ›" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Today" })).toBeDisabled();
  });

  it("explains a day with nothing on it", async () => {
    backend();
    mockCommand("day_report", () => report([]));
    render(<DayView date="2020-01-01" onChangeDate={() => {}} revision={0} />);

    expect(await screen.findAllByText(/Nothing was recorded on this day/)).toHaveLength(2);
    expect(screen.getByText(/No summary has been written/)).toBeInTheDocument();
  });
});
