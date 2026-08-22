import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import Search from "./Search";
import { clearMocks, mockCommand, type SearchHit } from "../lib/ipc";
import { backend, hit } from "../test/fixtures";

afterEach(() => {
  clearMocks();
  vi.restoreAllMocks();
});

describe("Search", () => {
  it("asks nothing until something is typed", async () => {
    backend();
    let asked = 0;
    mockCommand("search_history", (): SearchHit[] => {
      asked += 1;
      return [];
    });
    render(<Search onOpenDay={() => {}} />);

    expect(asked).toBe(0);
    expect(screen.queryByText(/Nothing matched/)).not.toBeInTheDocument();
  });

  it("sends one query for a burst of typing", async () => {
    backend();
    const queries: string[] = [];
    mockCommand("search_history", (args): SearchHit[] => {
      queries.push(String(args?.query ?? ""));
      return [hit()];
    });
    render(<Search onOpenDay={() => {}} />);

    const user = userEvent.setup();
    await user.type(screen.getByRole("searchbox", { name: "Search history" }), "code");

    await screen.findByText("Visual Studio Code");
    expect(queries).toEqual(["code"]);
  });

  it("shows what matched, with the day and the time", async () => {
    backend();
    mockCommand("search_history", () => [
      hit({ id: "a", app: "Visual Studio Code", title: "collector.rs", date: "2026-08-21" }),
    ]);
    render(<Search onOpenDay={() => {}} />);

    const user = userEvent.setup();
    await user.type(screen.getByRole("searchbox", { name: "Search history" }), "code");

    expect(await screen.findByText("collector.rs")).toBeInTheDocument();
    expect(screen.getByText(/2026-08-21/)).toBeInTheDocument();
    expect(screen.getByText("1 match")).toBeInTheDocument();
  });

  it("never shows a title for a private result", async () => {
    backend();
    const secret = hit({ id: "p", app: "Google Chrome", isPrivate: true });
    delete secret.title;
    mockCommand("search_history", () => [secret]);
    render(<Search onOpenDay={() => {}} />);

    const user = userEvent.setup();
    await user.type(screen.getByRole("searchbox", { name: "Search history" }), "chrome");

    expect(await screen.findByText(/title not recorded/)).toBeInTheDocument();
    expect(screen.queryByText("collector.rs - openhistory-win")).not.toBeInTheDocument();
  });

  it("says so when nothing matched", async () => {
    backend();
    mockCommand("search_history", (): SearchHit[] => []);
    render(<Search onOpenDay={() => {}} />);

    const user = userEvent.setup();
    await user.type(screen.getByRole("searchbox", { name: "Search history" }), "nothing");

    expect(await screen.findByText(/Nothing matched every word/)).toBeInTheDocument();
  });

  it("opens the day a result came from", async () => {
    backend();
    mockCommand("search_history", () => [hit({ date: "2026-08-19" })]);
    const opened: string[] = [];
    render(<Search onOpenDay={(date) => opened.push(date)} />);

    const user = userEvent.setup();
    await user.type(screen.getByRole("searchbox", { name: "Search history" }), "code");
    await user.click(await screen.findByRole("button", { name: /Visual Studio Code/ }));

    expect(opened).toEqual(["2026-08-19"]);
  });

  it("reports a failed search rather than showing stale results", async () => {
    backend();
    mockCommand("search_history", () => {
      throw new Error("the index is being rebuilt");
    });
    render(<Search onOpenDay={() => {}} />);

    const user = userEvent.setup();
    await user.type(screen.getByRole("searchbox", { name: "Search history" }), "code");

    expect(await screen.findByRole("alert")).toHaveTextContent("the index is being rebuilt");
  });
});
