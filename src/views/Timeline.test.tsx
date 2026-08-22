import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import Timeline from "./Timeline";
import { clearMocks, localDate, mockCommand } from "../lib/ipc";
import { MINUTE, backend, episode, report } from "../test/fixtures";

afterEach(() => {
  clearMocks();
  vi.restoreAllMocks();
});

/** A timestamp at a given local hour today, so the grouping is time-zone independent. */
function atHour(hour: number, minute = 0): string {
  const when = new Date();
  when.setHours(hour, minute, 0, 0);
  return when.toISOString();
}

describe("Timeline", () => {
  it("asks for today's local date", async () => {
    backend();
    const seen: unknown[] = [];
    mockCommand("day_report", (args) => {
      seen.push(args?.date);
      return report();
    });
    render(<Timeline revision={0} onOpenDay={() => {}} />);

    await waitFor(() => expect(seen).toContain(localDate()));
  });

  it("groups episodes by the hour they started, most recent hour first", async () => {
    backend();
    mockCommand("day_report", () =>
      report([
        episode({ id: "a", start: atHour(9), title: "earlier" }),
        episode({ id: "b", start: atHour(17), title: "later" }),
      ]),
    );
    render(<Timeline revision={0} onOpenDay={() => {}} />);

    const hours = await screen.findAllByRole("region", { name: /^\d\d:00$/ });
    expect(hours.map((one) => one.getAttribute("aria-label"))).toEqual(["17:00", "09:00"]);
    expect(within(hours[0]!).getByText("later")).toBeInTheDocument();
    expect(within(hours[1]!).getByText("earlier")).toBeInTheDocument();
  });

  it("keeps one episode in one hour even when it crosses the boundary", async () => {
    backend();
    mockCommand("day_report", () =>
      report([
        episode({
          id: "a",
          start: atHour(10, 50),
          end: atHour(11, 20),
          title: "one long stretch",
        }),
      ]),
    );
    render(<Timeline revision={0} onOpenDay={() => {}} />);

    const hours = await screen.findAllByRole("region", { name: /^\d\d:00$/ });
    expect(hours).toHaveLength(1);
    expect(hours[0]).toHaveAttribute("aria-label", "10:00");
  });

  it("measures each episode and the day as a whole", async () => {
    backend();
    mockCommand("day_report", () =>
      report([
        episode({ id: "a", app: "Visual Studio Code", activeMs: 90 * MINUTE, start: atHour(9) }),
        episode({ id: "b", app: "Google Chrome", activeMs: 30 * MINUTE, start: atHour(11) }),
      ]),
    );
    render(<Timeline revision={0} onOpenDay={() => {}} />);

    expect(await screen.findByText(/2h active/)).toBeInTheDocument();
    expect(screen.getByText(/2 episodes/)).toBeInTheDocument();
    expect(screen.getByText(/mostly Visual Studio Code/)).toBeInTheDocument();
    expect(screen.getAllByText("1h 30m").length).toBeGreaterThan(0);
  });

  it("names a private session without showing anything about it", async () => {
    const secret = episode({ id: "p1", app: "Google Chrome", isPrivate: true, start: atHour(14) });
    delete secret.title;
    backend();
    mockCommand("day_report", () => report([secret]));
    render(<Timeline revision={0} onOpenDay={() => {}} />);

    expect(await screen.findByText(/Private browsing/)).toBeInTheDocument();
    expect(screen.getByText("Google Chrome")).toBeInTheDocument();
    expect(screen.getByText(/1 private session/)).toBeInTheDocument();
  });

  it("expands an episode to the other windows seen during it", async () => {
    backend();
    mockCommand("day_report", () =>
      report([
        episode({
          id: "a",
          start: atHour(9),
          title: "collector.rs",
          titles: ["collector.rs", "store.rs", "day.rs"],
        }),
      ]),
    );
    render(<Timeline revision={0} onOpenDay={() => {}} />);

    const user = userEvent.setup();
    const more = await screen.findByRole("button", { name: "2 more windows" });
    expect(more).toHaveAttribute("aria-expanded", "false");

    await user.click(more);
    expect(screen.getByText("store.rs")).toBeInTheDocument();
    expect(screen.getByText("day.rs")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Hide windows" }));
    expect(screen.queryByText("store.rs")).not.toBeInTheDocument();
  });

  it("explains an empty day rather than showing a bare list", async () => {
    backend();
    mockCommand("day_report", () => report([]));
    render(<Timeline revision={0} onOpenDay={() => {}} />);

    expect(await screen.findByText(/Nothing recorded yet today/)).toBeInTheDocument();
    expect(screen.queryAllByRole("listitem")).toHaveLength(0);
  });

  it("reloads when the shell says the history has moved", async () => {
    let day = report([episode({ id: "a", title: "first", start: atHour(9) })]);
    backend();
    mockCommand("day_report", () => day);
    const { rerender } = render(<Timeline revision={0} onOpenDay={() => {}} />);
    await screen.findByText("first");

    day = report([
      episode({ id: "a", title: "first", start: atHour(9) }),
      episode({ id: "b", title: "second", start: atHour(10) }),
    ]);
    rerender(<Timeline revision={1} onOpenDay={() => {}} />);

    expect(await screen.findByText("second")).toBeInTheDocument();
  });

  it("hands the day view today's date", async () => {
    backend();
    const opened: string[] = [];
    render(<Timeline revision={0} onOpenDay={(date) => opened.push(date)} />);

    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Open day view" }));
    expect(opened).toEqual([localDate()]);
  });
});
