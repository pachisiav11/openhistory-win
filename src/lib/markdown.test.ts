import { describe, expect, it } from "vitest";
import { parseMarkdown } from "./markdown";

describe("Markdown", () => {
  it("reads a document of the shape the backend writes", () => {
    const document = [
      "# Saturday 22 August 2026",
      "",
      "A morning of Rust and a short read.",
      "",
      "## Where the time went",
      "",
      "1h at the machine, 45m of it working.",
      "",
      "- Visual Studio Code — 45m",
      "- Idle — 15m",
      "",
      "---",
      "",
      "Summaries written by claude-haiku-4-5.",
    ].join("\n");

    expect(parseMarkdown(document)).toEqual([
      { kind: "heading", level: 1, text: "Saturday 22 August 2026" },
      { kind: "paragraph", text: "A morning of Rust and a short read." },
      { kind: "heading", level: 2, text: "Where the time went" },
      { kind: "paragraph", text: "1h at the machine, 45m of it working." },
      { kind: "list", items: ["Visual Studio Code — 45m", "Idle — 15m"] },
      { kind: "rule" },
      { kind: "paragraph", text: "Summaries written by claude-haiku-4-5." },
    ]);
  });

  it("joins the lines of one paragraph and separates two", () => {
    expect(parseMarkdown("one\ntwo\n\nthree")).toEqual([
      { kind: "paragraph", text: "one two" },
      { kind: "paragraph", text: "three" },
    ]);
  });

  it("ends a list when the prose starts again", () => {
    expect(parseMarkdown("- a\n- b\nafter")).toEqual([
      { kind: "list", items: ["a", "b"] },
      { kind: "paragraph", text: "after" },
    ]);
  });

  it("does not nest headings deeper than there are levels to render", () => {
    const parsed = parseMarkdown("### three\n\n##### five");
    expect(parsed).toEqual([
      { kind: "heading", level: 3, text: "three" },
      { kind: "heading", level: 3, text: "five" },
    ]);
  });

  it("treats an unrecognised line as prose rather than dropping it", () => {
    expect(parseMarkdown("| a | b |\n\n> quoted")).toEqual([
      { kind: "paragraph", text: "| a | b |" },
      { kind: "paragraph", text: "> quoted" },
    ]);
  });

  it("reads an empty document as nothing at all", () => {
    expect(parseMarkdown("")).toEqual([]);
    expect(parseMarkdown("\n\n   \n")).toEqual([]);
  });
});
