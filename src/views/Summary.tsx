/**
 * A day's whole summary, and the documents kept from it.
 *
 * Everything else the application shows is derived: forget a day, or let the retention
 * window pass, and the numbers behind it are gone. A document saved here is the one
 * thing that stays. That is why saving is a deliberate button rather than something the
 * application does on its own, and why removing one asks first.
 *
 * The Day view still owns the writing — this page reads what was written and keeps it.
 */
import { useCallback, useEffect, useState } from "react";
import {
  daySummary,
  libraryDelete,
  libraryDocument,
  libraryEntries,
  libraryExport,
  librarySave,
  localDate,
  type DaySummary,
  type LibraryEntry,
} from "../lib/ipc";
import { clockTime, fileSize, shiftDate } from "../lib/format";
import { parseMarkdown, type Block } from "../lib/markdown";

interface Props {
  date: string;
  onChangeDate: (date: string) => void;
  revision: number;
}

/** When a document was saved, in the reader's own day and clock. */
function stamp(savedAt: string): string {
  const when = new Date(savedAt);
  return Number.isNaN(when.getTime())
    ? "an unknown time"
    : `${localDate(when)} at ${clockTime(savedAt)}`;
}

function renderBlock(block: Block, index: number) {
  switch (block.kind) {
    case "heading":
      if (block.level === 1) return <h4 key={index} className="document__h1">{block.text}</h4>;
      if (block.level === 2) return <h5 key={index} className="document__h2">{block.text}</h5>;
      return <h6 key={index} className="document__h3">{block.text}</h6>;
    case "list":
      return (
        <ul key={index} className="document__list">
          {block.items.map((item, n) => (
            <li key={n}>{item}</li>
          ))}
        </ul>
      );
    case "rule":
      return <hr key={index} className="document__rule" />;
    default:
      return (
        <p key={index} className="document__p">
          {block.text}
        </p>
      );
  }
}

export default function Summary({ date, onChangeDate, revision }: Props) {
  const [entries, setEntries] = useState<LibraryEntry[]>([]);
  const [summary, setSummary] = useState<DaySummary | null>(null);
  const [reading, setReading] = useState<{ entry: LibraryEntry; body: string } | null>(null);
  const [confirming, setConfirming] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);

  const today = localDate();

  const fail = useCallback((cause: unknown) => {
    setError(cause instanceof Error ? cause.message : String(cause));
  }, []);

  const list = useCallback(() => {
    libraryEntries().then(setEntries).catch(fail);
  }, [fail]);

  useEffect(list, [list, revision]);
  useEffect(() => {
    daySummary(date).then(setSummary).catch(fail);
  }, [date, revision, fail]);

  const run = useCallback(
    async (label: string, work: () => Promise<string | null>) => {
      setBusy(label);
      setError(null);
      setNote(null);
      try {
        setNote(await work());
      } catch (cause) {
        fail(cause);
      } finally {
        setBusy(null);
      }
    },
    [fail],
  );

  const save = () =>
    run("save", async () => {
      const entry = await librarySave(date);
      const body = await libraryDocument(entry.id);
      setReading({ entry, body });
      list();
      return `Saved ${entry.title} to the library.`;
    });

  const read = (entry: LibraryEntry) =>
    run(`read-${entry.id}`, async () => {
      setReading({ entry, body: await libraryDocument(entry.id) });
      return null;
    });

  const remove = (entry: LibraryEntry) =>
    run(`delete-${entry.id}`, async () => {
      await libraryDelete(entry.id);
      setConfirming(null);
      setReading((open) => (open?.entry.id === entry.id ? null : open));
      list();
      return `Removed ${entry.title} from the library.`;
    });

  const exportCopy = (entry: LibraryEntry) =>
    run(`export-${entry.id}`, async () => {
      const path = await libraryExport(entry.id);
      // Dismissing the dialog is an ordinary thing to do, so it says nothing at all.
      return path === null ? null : `Wrote a copy to ${path}.`;
    });

  const working = busy !== null;

  return (
    <section aria-label="Summary view">
      <div className="section__head">
        <h2 className="section__title">{date === today ? `Today · ${date}` : date}</h2>
        <div className="daynav">
          <button
            type="button"
            className="button button--quiet"
            onClick={() => onChangeDate(shiftDate(date, -1))}
          >
            ‹ Previous
          </button>
          <button
            type="button"
            className="button button--quiet"
            onClick={() => onChangeDate(today)}
            disabled={date === today}
          >
            Today
          </button>
          <button
            type="button"
            className="button button--quiet"
            onClick={() => onChangeDate(shiftDate(date, 1))}
            disabled={date >= today}
          >
            Next ›
          </button>
        </div>
      </div>

      {error ? (
        <p className="notice notice--error" role="alert">
          {error}
        </p>
      ) : null}
      {note ? <p className="notice notice--ok">{note}</p> : null}

      <section className="panel" aria-label="This day">
        <div className="panel__head">
          <h3 className="panel__title">This day</h3>
          <div className="panel__actions">
            <button type="button" className="button" disabled={working} onClick={save}>
              {busy === "save" ? "Saving…" : "Save to the library"}
            </button>
          </div>
        </div>

        {summary?.daily ? (
          <p className="summary__text">{summary.daily}</p>
        ) : (
          <p className="empty">
            No summary has been written for this day. Saving it still keeps the hours and where
            the time went.
          </p>
        )}

        <p className="panel__hint">
          A saved day is a Markdown file kept beside the history. It stays after the events
          behind it have been forgotten, and the Day view can write a new summary at any time.
        </p>
      </section>

      <section className="panel" aria-label="Library">
        <h3 className="panel__title">Library</h3>
        {entries.length === 0 ? (
          <p className="empty">Nothing has been saved yet.</p>
        ) : (
          <ul className="library">
            {entries.map((entry) => (
              <li key={entry.id} className="saved">
                <div className="saved__body">
                  <p className="saved__title">{entry.title}</p>
                  <p className="saved__meta">
                    Saved {stamp(entry.savedAt)} · {fileSize(entry.bytes)}
                  </p>
                </div>
                <div className="saved__actions">
                  <button
                    type="button"
                    className="button button--quiet"
                    disabled={working}
                    onClick={() => read(entry)}
                  >
                    Read
                  </button>
                  <button
                    type="button"
                    className="button button--quiet"
                    disabled={working}
                    onClick={() => exportCopy(entry)}
                  >
                    Export
                  </button>
                  <button
                    type="button"
                    className="button button--quiet"
                    disabled={working}
                    onClick={() => (confirming === entry.id ? remove(entry) : setConfirming(entry.id))}
                  >
                    {confirming === entry.id ? "Remove for good" : "Remove"}
                  </button>
                </div>
              </li>
            ))}
          </ul>
        )}
      </section>

      {reading ? (
        <section className="panel" aria-label="Saved document">
          <div className="panel__head">
            <h3 className="panel__title">{reading.entry.title}</h3>
            <div className="panel__actions">
              <button
                type="button"
                className="button button--quiet"
                onClick={() => setReading(null)}
              >
                Close
              </button>
            </div>
          </div>
          <article className="document">{parseMarkdown(reading.body).map(renderBlock)}</article>
        </section>
      ) : null}
    </section>
  );
}
