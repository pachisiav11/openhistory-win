import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
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

  it("keeps the three paragraphs a day summary is written in", async () => {
    backend();
    mockCommand("day_summary", () =>
      summary({
        date: "2026-08-21",
        daily: "What was done.\n\nWhat it means.\n\nIn conclusion, a good day.",
      }),
    );
    render(<DayView date="2026-08-21" onChangeDate={() => {}} revision={0} />);

    const first = await screen.findByText("What was done.");
    expect(first.tagName).toBe("P");
    expect(screen.getByText("What it means.")).toBeInTheDocument();
    expect(screen.getByText("In conclusion, a good day.")).toBeInTheDocument();
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

    // Beside both things it blocks: writing a summary, and asking about the day.
    expect(await screen.findAllByText(/need your agreement/)).toHaveLength(2);
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

  it("offers to write an hour again once it already has a summary", async () => {
    // An hour summarized while it was still filling describes only the part that had
    // happened by then. The control to correct it used to disappear the moment a
    // summary existed, leaving the first attempt standing for good.
    backend({ readiness: READY });
    mockCommand("day_report", () => report([episode()], hours()));
    const asked: number[] = [];
    mockCommand("day_summary", () => ({
      date: "2026-08-21",
      hours: [
        {
          hour: 9,
          text: "Half an hour that was not over yet.",
          activeMs: 0,
          generatedAt: "",
          provider: "anthropic",
          model: "claude-haiku-4-5",
        },
      ],
    }));
    mockCommand("summarize_hour", (args) => {
      asked.push(Number(args?.hour));
      return {
        hour: Number(args?.hour),
        text: "The whole hour, this time.",
        activeMs: 0,
        generatedAt: "",
        provider: "anthropic",
        model: "claude-haiku-4-5",
      };
    });
    render(<DayView date="2026-08-21" onChangeDate={() => {}} revision={0} />);

    expect(await screen.findByText("Half an hour that was not over yet.")).toBeInTheDocument();
    await userEvent.setup().click(await screen.findByRole("button", { name: "Write again" }));

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

  it("marks the hour a search result was opened at, and says it has arrived", async () => {
    backend();
    mockCommand("day_report", () => report([episode()], hours()));
    let told = 0;
    // Stable, as the shell's is: a fresh callback each render would ask the effect to
    // run again on every unrelated render.
    const arrived = () => {
      told += 1;
    };
    render(
      <DayView
        date="2026-08-21"
        onChangeDate={() => {}}
        revision={0}
        focusHour={10}
        onFocused={arrived}
      />,
    );

    // The mark is applied once the day has loaded, a commit after the hours appear.
    await waitFor(() =>
      expect(screen.getByText("10:00").closest("li")).toHaveAttribute("aria-current", "location"),
    );
    expect(screen.getByText("09:00").closest("li")).not.toHaveAttribute("aria-current");
    expect(told).toBe(1);
  });

  it("marks no hour when the day was opened without one", async () => {
    backend();
    mockCommand("day_report", () => report([episode()], hours()));
    render(<DayView date="2026-08-21" onChangeDate={() => {}} revision={0} focusHour={null} />);

    expect((await screen.findByText("09:00")).closest("li")).not.toHaveAttribute("aria-current");
    expect(screen.getByText("10:00").closest("li")).not.toHaveAttribute("aria-current");
  });

  it("counts idle time as screen time without crediting it to an application", async () => {
    backend();
    mockCommand("day_report", () =>
      report(
        [
          episode({
            id: "a",
            app: "Visual Studio Code",
            durationMs: 60 * MINUTE,
            activeMs: 45 * MINUTE,
          }),
        ],
        hours(),
      ),
    );
    render(<DayView date="2026-08-21" onChangeDate={() => {}} revision={0} />);

    expect(await screen.findByText(/1h at the machine · 45m of it working/)).toBeInTheDocument();

    const idle = screen.getByText("Idle").closest("li");
    expect(idle).toHaveTextContent("15m");
    // The editor keeps its 45 minutes: the idle quarter of an hour is nobody's.
    expect(screen.getByText("Visual Studio Code").closest("li")).toHaveTextContent("45m");
  });

  it("shows no idle row for a day with evidence for all of it", async () => {
    backend();
    mockCommand("day_report", () =>
      report([episode({ durationMs: 30 * MINUTE, activeMs: 30 * MINUTE })], hours()),
    );
    render(<DayView date="2026-08-21" onChangeDate={() => {}} revision={0} />);

    expect(await screen.findByText(/30m at the machine · 30m of it working/)).toBeInTheDocument();
    expect(screen.queryByText("Idle")).not.toBeInTheDocument();
  });

  it("leaves an application in front for under a minute out of the list", async () => {
    backend();
    mockCommand("day_report", () =>
      report(
        [
          episode({ id: "a", app: "Visual Studio Code", activeMs: 45 * MINUTE }),
          episode({ id: "b", app: "Calculator", activeMs: 20 * 1000 }),
        ],
        hours(),
      ),
    );
    render(<DayView date="2026-08-21" onChangeDate={() => {}} revision={0} />);

    expect(await screen.findByText("Visual Studio Code")).toBeInTheDocument();
    expect(screen.queryByText("Calculator")).not.toBeInTheDocument();
    // Hidden, not discarded: the panel says the time is still in the totals.
    expect(
      screen.getByText(/1 application in front for under a minute is counted in the totals/),
    ).toBeInTheDocument();
  });

  it("says so when nothing was in front for longer than a minute", async () => {
    backend();
    mockCommand("day_report", () =>
      report(
        [
          episode({ id: "a", app: "Calculator", durationMs: 20 * 1000, activeMs: 20 * 1000 }),
        ],
        hours(),
      ),
    );
    render(<DayView date="2026-08-21" onChangeDate={() => {}} revision={0} />);

    expect(
      await screen.findByText(/Nothing was in front for longer than a minute/),
    ).toBeInTheDocument();
  });

  it("answers a question about the day", async () => {
    backend({ readiness: READY });
    mockCommand("chat_about_day", (args) => ({
      text: `The morning went to ${String(args?.date)}.`,
      model: "claude-haiku-4-5",
    }));
    render(<DayView date="2026-08-21" onChangeDate={() => {}} revision={0} />);

    const box = await screen.findByLabelText("Your question");
    await userEvent.type(box, "What took the morning?");
    await userEvent.click(screen.getByRole("button", { name: "Ask" }));

    expect(await screen.findByText("What took the morning?")).toBeInTheDocument();
    expect(screen.getByText("The morning went to 2026-08-21.")).toBeInTheDocument();
    // Which model answered, not which one is selected now.
    expect(screen.getByText("Answered by claude-haiku-4-5.")).toBeInTheDocument();
    // The box empties, so the next question is not typed onto the end of the last.
    expect(box).toHaveValue("");
  });

  /// Nothing about a conversation is stored, so the transcript is the only memory it
  /// has and it has to travel back with the question.
  it("carries the conversation so far back with the next question", async () => {
    const sent: unknown[] = [];
    backend({ readiness: READY });
    mockCommand("chat_about_day", (args) => {
      sent.push(args?.turns);
      return { text: `Answer ${sent.length}.`, model: "claude-haiku-4-5" };
    });
    render(<DayView date="2026-08-21" onChangeDate={() => {}} revision={0} />);

    const box = await screen.findByLabelText("Your question");
    await userEvent.type(box, "First?");
    await userEvent.click(screen.getByRole("button", { name: "Ask" }));
    expect(await screen.findByText("Answer 1.")).toBeInTheDocument();

    await userEvent.type(box, "Second?");
    await userEvent.click(screen.getByRole("button", { name: "Ask" }));
    expect(await screen.findByText("Answer 2.")).toBeInTheDocument();

    expect(sent[0]).toEqual([]);
    expect(sent[1]).toEqual([{ asked: "First?", answered: "Answer 1." }]);
  });

  it("starts a new conversation when the day changes", async () => {
    backend({ readiness: READY });
    mockCommand("chat_about_day", () => ({ text: "It went well.", model: "claude-haiku-4-5" }));
    const { rerender } = render(
      <DayView date="2026-08-21" onChangeDate={() => {}} revision={0} />,
    );

    const box = await screen.findByLabelText("Your question");
    await userEvent.type(box, "What happened?");
    await userEvent.click(screen.getByRole("button", { name: "Ask" }));
    expect(await screen.findByText("It went well.")).toBeInTheDocument();

    rerender(<DayView date="2026-08-20" onChangeDate={() => {}} revision={0} />);

    await waitFor(() => expect(screen.queryByText("It went well.")).not.toBeInTheDocument());
  });

  it("reports a failure to answer instead of showing an empty reply", async () => {
    backend({ readiness: READY });
    mockCommand("chat_about_day", () => {
      throw new Error("google stopped early: it reached its token ceiling");
    });
    render(<DayView date="2026-08-21" onChangeDate={() => {}} revision={0} />);

    const box = await screen.findByLabelText("Your question");
    await userEvent.type(box, "What happened?");
    await userEvent.click(screen.getByRole("button", { name: "Ask" }));

    expect(await screen.findByText(/reached its token ceiling/)).toBeInTheDocument();
  });

  it("cannot be asked anything while no model is configured", async () => {
    backend();
    render(<DayView date="2026-08-21" onChangeDate={() => {}} revision={0} />);

    expect(await screen.findByLabelText("Your question")).toBeDisabled();
    expect(screen.getByRole("button", { name: "Ask" })).toBeDisabled();
  });

  it("explains a day with nothing on it", async () => {
    backend();
    mockCommand("day_report", () => report([]));
    render(<DayView date="2020-01-01" onChangeDate={() => {}} revision={0} />);

    expect(await screen.findAllByText(/Nothing was recorded on this day/)).toHaveLength(2);
    expect(screen.getByText(/No summary has been written/)).toBeInTheDocument();
  });
});
