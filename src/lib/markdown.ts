/**
 * Just enough Markdown to read a saved summary.
 *
 * The documents this parses are written by [`compose`](src-tauri/src/library.rs), so
 * the grammar is not general: headings, bullet lists, paragraphs and a rule are all
 * that is ever produced. A full Markdown library would be a dependency carried for a
 * file the application wrote itself, and the reason the library stores Markdown at all
 * is that the file is legible without one.
 *
 * A document somebody edited by hand still renders — anything unrecognised is a
 * paragraph, which is what a reader wants from an unknown line.
 */

export type Block =
  | { kind: "heading"; level: 1 | 2 | 3; text: string }
  | { kind: "paragraph"; text: string }
  | { kind: "list"; items: string[] }
  | { kind: "rule" };

/** Split a document into the blocks a reader sees. */
export function parseMarkdown(source: string): Block[] {
  const blocks: Block[] = [];
  let paragraph: string[] = [];
  let items: string[] = [];

  const endParagraph = () => {
    if (paragraph.length > 0) {
      blocks.push({ kind: "paragraph", text: paragraph.join(" ") });
      paragraph = [];
    }
  };
  const endList = () => {
    if (items.length > 0) {
      blocks.push({ kind: "list", items });
      items = [];
    }
  };
  const endBoth = () => {
    endParagraph();
    endList();
  };

  for (const raw of source.split(/\r?\n/)) {
    const line = raw.trim();

    if (line === "") {
      endBoth();
      continue;
    }
    if (/^-{3,}$/.test(line)) {
      endBoth();
      blocks.push({ kind: "rule" });
      continue;
    }

    const heading = /^(#{1,6})\s+(.*)$/.exec(line);
    if (heading) {
      endBoth();
      const depth = heading[1]!.length;
      blocks.push({
        kind: "heading",
        level: depth >= 3 ? 3 : (depth as 1 | 2),
        text: heading[2]!.trim(),
      });
      continue;
    }

    const bullet = /^[-*]\s+(.*)$/.exec(line);
    if (bullet) {
      endParagraph();
      items.push(bullet[1]!.trim());
      continue;
    }

    endList();
    paragraph.push(line);
  }

  endBoth();
  return blocks;
}
