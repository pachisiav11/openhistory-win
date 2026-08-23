import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import Summary from "./Summary";
import { clearMocks, mockCommand, type LibraryEntry } from "../lib/ipc";
import { backend, libraryEntry, summary } from "../test/fixtures";

afterEach(() => {
  clearMocks();
  vi.restoreAllMocks();
});

const DOCUMENT = [
  "# Friday 21 August 2026",
  "",
  "A morning of Rust and a short read.",
  "",
  "## Where the time went",
  "",
  "- Visual Studio Code — 45m",
].join("\n");

function open(date = "2026-08-21") {
  render(<Summary date={date} onChangeDate={() => {}} revision={0} />);
}

describe("Summary view", () => {
  it("shows the day's summary beside the button that keeps it", async () => {
    backend();
    mockCommand("day_summary", () =>
      summary({ date: "2026-08-21", daily: "A morning of Rust and a short read." }),
    );
    open();

    expect(await screen.findByText("A morning of Rust and a short read.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Save to the library" })).toBeEnabled();
  });

  it("says a day with no summary is still worth keeping", async () => {
    backend();
    open();

    expect(await screen.findByText(/No summary has been written/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Save to the library" })).toBeEnabled();
  });

  it("says when nothing has been saved yet", async () => {
    backend();
    open();

    expect(await screen.findByText("Nothing has been saved yet.")).toBeInTheDocument();
  });

  it("lists what the library holds", async () => {
    backend();
    mockCommand("library_entries", (): LibraryEntry[] => [
      libraryEntry({ id: "2026-08-21", title: "Friday 21 August 2026", bytes: 2400 }),
      libraryEntry({ id: "2026-08-20", title: "Thursday 20 August 2026", bytes: 800 }),
    ]);
    open();

    expect(await screen.findByText("Friday 21 August 2026")).toBeInTheDocument();
    expect(screen.getByText("Thursday 20 August 2026")).toBeInTheDocument();
    expect(screen.getByText(/2\.4 kB/)).toBeInTheDocument();
    expect(screen.getByText(/800 bytes/)).toBeInTheDocument();
  });

  it("saves the day and shows what it wrote", async () => {
    backend();
    const kept: LibraryEntry[] = [];
    mockCommand("library_entries", () => [...kept]);
    mockCommand("library_save", (args): LibraryEntry => {
      const entry = libraryEntry({ id: String(args?.date), date: String(args?.date) });
      kept.push(entry);
      return entry;
    });
    mockCommand("library_document", () => DOCUMENT);
    open();

    await screen.findByText("Nothing has been saved yet.");
    await userEvent.click(screen.getByRole("button", { name: "Save to the library" }));

    expect(await screen.findByText(/Saved Friday 21 August 2026 to the library/)).toBeInTheDocument();
    expect(screen.getByText("Where the time went")).toBeInTheDocument();
    expect(screen.getByText("Visual Studio Code — 45m")).toBeInTheDocument();
  });

  it("reads a saved document into the page", async () => {
    backend();
    mockCommand("library_entries", (): LibraryEntry[] => [libraryEntry()]);
    mockCommand("library_document", () => DOCUMENT);
    open();

    await userEvent.click(await screen.findByRole("button", { name: "Read" }));

    expect(await screen.findByText("A morning of Rust and a short read.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Close" })).toBeInTheDocument();
  });

  it("asks before it removes a document for good", async () => {
    backend();
    let kept: LibraryEntry[] = [libraryEntry()];
    mockCommand("library_entries", () => [...kept]);
    const deleted = vi.fn(() => {
      kept = [];
    });
    mockCommand("library_delete", deleted);
    open();

    await userEvent.click(await screen.findByRole("button", { name: "Remove" }));
    expect(deleted).not.toHaveBeenCalled();

    await userEvent.click(screen.getByRole("button", { name: "Remove for good" }));

    expect(deleted).toHaveBeenCalled();
    expect(await screen.findByText("Nothing has been saved yet.")).toBeInTheDocument();
  });

  it("says where an exported copy went, and nothing when the dialog is dismissed", async () => {
    backend();
    mockCommand("library_entries", (): LibraryEntry[] => [libraryEntry()]);
    mockCommand("library_export", () => String.raw`C:\Users\you\Desktop\2026-08-21.md`);
    open();

    await userEvent.click(await screen.findByRole("button", { name: "Export" }));
    expect(await screen.findByText(/Wrote a copy to/)).toBeInTheDocument();

    mockCommand("library_export", () => null);
    await userEvent.click(screen.getByRole("button", { name: "Export" }));
    await waitFor(() => expect(screen.queryByText(/Wrote a copy to/)).not.toBeInTheDocument());
  });

  it("reports a failed export rather than pretending it worked", async () => {
    backend();
    mockCommand("library_entries", (): LibraryEntry[] => [libraryEntry()]);
    mockCommand("library_export", () => {
      throw new Error("exporting needs the desktop application");
    });
    open();

    await userEvent.click(await screen.findByRole("button", { name: "Export" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "exporting needs the desktop application",
    );
  });
});
