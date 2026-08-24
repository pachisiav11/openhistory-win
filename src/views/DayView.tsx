/**
 * One day at a glance: what was written about it, how the hours went, and where the
 * time went.
 *
 * The summary buttons are disabled with the backend's own reason next to them rather
 * than hidden. A person who has not set up a model should be able to see that the
 * feature exists and what it needs, not wonder where it went.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import {
  dayReport,
  daySummary,
  forgetSummary,
  inferenceReadiness,
  localDate,
  summarizeDay,
  summarizeHour,
  type DayReport,
  type DaySummary,
  type Readiness,
} from "../lib/ipc";
import { duration, shiftDate } from "../lib/format";

interface Props {
  date: string;
  onChangeDate: (date: string) => void;
  revision: number;
  /** An hour to scroll to and mark, set when the day was opened from a search result. */
  focusHour?: number | null;
  /** Called once the hour has been reached, so the same request can be made again. */
  onFocused?: () => void;
  /** Open the Summary view on this day. Absent when there is nowhere to open it. */
  onOpenSummary?: () => void;
}

function hourLabel(hour: number): string {
  return `${String(hour).padStart(2, "0")}:00`;
}

export default function DayView({
  date,
  onChangeDate,
  revision,
  focusHour,
  onFocused,
  onOpenSummary,
}: Props) {
  const [report, setReport] = useState<DayReport | null>(null);
  const [summary, setSummary] = useState<DaySummary | null>(null);
  const [readiness, setReadiness] = useState<Readiness | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);

  const today = localDate();

  const load = useCallback(() => {
    dayReport(date).then(setReport).catch(setError);
    daySummary(date).then(setSummary).catch(setError);
  }, [date]);

  useEffect(load, [load, revision]);
  useEffect(() => {
    inferenceReadiness().then(setReadiness).catch(setError);
  }, [revision]);

  // Marking an hour outlives the request that asked for it, so the mark is state here
  // rather than the prop itself. Declared before the effect that sets it, so opening
  // another day and an hour of it in the same click leaves the hour marked.
  const [marked, setMarked] = useState<number | null>(null);
  useEffect(() => setMarked(null), [date]);

  const hourNodes = useRef(new Map<number, HTMLLIElement>());
  useEffect(() => {
    if (focusHour === null || focusHour === undefined || report === null) return;
    setMarked(focusHour);
    hourNodes.current.get(focusHour)?.scrollIntoView({ block: "center" });
    onFocused?.();
  }, [focusHour, report, onFocused]);

  const run = useCallback(
    async (label: string, work: () => Promise<unknown>) => {
      setBusy(label);
      setError(null);
      setNote(null);
      try {
        await work();
        setSummary(await daySummary(date));
      } catch (cause) {
        setError(cause instanceof Error ? cause.message : String(cause));
      } finally {
        setBusy(null);
      }
    },
    [date],
  );

  const write = (rewrite: boolean) =>
    run(rewrite ? "rewrite" : "write", async () => {
      const outcome = await summarizeDay(date, rewrite);
      if (outcome.failure) throw new Error(outcome.failure);
      const written = outcome.hoursWritten.length;
      setNote(
        written === 0 && !outcome.dailyWritten
          ? "Nothing new to write: every hour with enough activity already has a summary."
          : `Wrote ${written} hour${written === 1 ? "" : "s"}${outcome.dailyWritten ? " and the day" : ""}.`,
      );
    });

  const ready = readiness?.ready ?? false;
  const hours = report?.rollup.hours ?? [];
  const busiest = hours.reduce((most, one) => Math.max(most, one.activeMs), 0);
  const written = new Map((summary?.hours ?? []).map((one) => [one.hour, one]));
  // Writing without forcing only fills the hours that have none, so once a day has
  // any summary the same button means something different from a rewrite.
  const anything = Boolean(summary && (summary.daily || summary.hours.length > 0));

  // The bars are shares of screen time, not of worked time, so the idle row is
  // measured against the same whole as the applications beside it.
  const idleMs = report?.rollup.idleMs ?? 0;
  const screenMs = (report?.rollup.activeMs ?? 0) + idleMs;
  const share = (ms: number) => (screenMs > 0 ? Math.round((ms / screenMs) * 100) : 0);

  return (
    <section aria-label="Day view">
      <div className="section__head">
        <div className="section__lead">
          <h2 className="section__title">{date === today ? `Today · ${date}` : date}</h2>
          {onOpenSummary ? (
            <button type="button" className="button button--quiet" onClick={onOpenSummary}>
              Summary page
            </button>
          ) : null}
        </div>
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

      <section className="panel" aria-label="Summary">
        <div className="panel__head">
          <h3 className="panel__title">Summary</h3>
          <div className="panel__actions">
            <button
              type="button"
              className="button"
              disabled={!ready || busy !== null}
              onClick={() => write(false)}
            >
              {busy === "write" ? "Writing…" : anything ? "Write new hours" : "Write summary"}
            </button>
            {anything ? (
              <>
                <button
                  type="button"
                  className="button button--quiet"
                  disabled={!ready || busy !== null}
                  onClick={() => write(true)}
                >
                  Rewrite
                </button>
                <button
                  type="button"
                  className="button button--quiet"
                  disabled={busy !== null}
                  onClick={() => run("forget", () => forgetSummary(date))}
                >
                  Forget
                </button>
              </>
            ) : null}
          </div>
        </div>

        {!ready && readiness?.blockedBy ? (
          <p className="panel__hint">{readiness.blockedBy}</p>
        ) : null}
        {ready && readiness?.model ? (
          <p className="panel__hint">Written by {readiness.model}.</p>
        ) : null}

        {summary?.daily ? (
          <p className="summary__text">{summary.daily}</p>
        ) : (
          <p className="empty">No summary has been written for this day.</p>
        )}
      </section>

      <section className="panel" aria-label="Hours">
        <h3 className="panel__title">Hours</h3>
        {hours.length === 0 ? (
          <p className="empty">Nothing was recorded on this day.</p>
        ) : (
          <ol className="hours">
            {hours.map((hour) => {
              const said = written.get(hour.hour);
              return (
                <li
                  key={hour.hour}
                  className={`hourbar${marked === hour.hour ? " hourbar--marked" : ""}`}
                  aria-current={marked === hour.hour ? "location" : undefined}
                  ref={(node) => {
                    if (node) hourNodes.current.set(hour.hour, node);
                    else hourNodes.current.delete(hour.hour);
                  }}
                >
                  <span className="hourbar__label">{hourLabel(hour.hour)}</span>
                  <span className="hourbar__track">
                    <span
                      className="hourbar__fill"
                      style={{
                        width: `${busiest > 0 ? Math.round((hour.activeMs / busiest) * 100) : 0}%`,
                      }}
                    />
                  </span>
                  <span className="hourbar__active">{duration(hour.activeMs)}</span>
                  <span className="hourbar__said">
                    {said ? said.text : null}
                    {/* An hour summarized while it was still filling describes only the
                        part that had happened, so the way to correct one has to be here
                        beside it — summarize_hour has always rewritten whatever was
                        there, but nothing in the interface could ask it to. */}
                    <button
                      type="button"
                      className="episode__more"
                      disabled={!ready || busy !== null}
                      onClick={() =>
                        run(`hour-${hour.hour}`, () => summarizeHour(date, hour.hour))
                      }
                    >
                      {busy === `hour-${hour.hour}`
                        ? "Writing…"
                        : said
                          ? "Write again"
                          : "Summarize this hour"}
                    </button>
                  </span>
                </li>
              );
            })}
          </ol>
        )}
      </section>

      <section className="panel" aria-label="Applications">
        <div className="panel__head">
          <h3 className="panel__title">Where the time went</h3>
          {screenMs > 0 ? (
            <p className="panel__total">
              {duration(screenMs)} at the machine · {duration(report?.rollup.activeMs ?? 0)} of it
              working
            </p>
          ) : null}
        </div>
        {report && report.rollup.apps.length > 0 ? (
          <>
            <ol className="apps">
              {report.rollup.apps.map((app) => (
                <li key={app.app} className="app">
                  <span className="app__name">{app.app}</span>
                  <span className="app__bar">
                    <span className="app__fill" style={{ width: `${share(app.activeMs)}%` }} />
                  </span>
                  <span className="app__active">{duration(app.activeMs)}</span>
                </li>
              ))}
              {idleMs > 0 ? (
                <li className="app app--idle">
                  <span className="app__name">Idle</span>
                  <span className="app__bar">
                    <span className="app__fill" style={{ width: `${share(idleMs)}%` }} />
                  </span>
                  <span className="app__active">{duration(idleMs)}</span>
                </li>
              ) : null}
            </ol>
            <p className="panel__hint">
              Idle is time a window sat in front with nothing happening. It belongs to no
              application. Time while the screen was locked or asleep is in neither figure.
            </p>
          </>
        ) : (
          <p className="empty">Nothing was recorded on this day.</p>
        )}
      </section>
    </section>
  );
}
