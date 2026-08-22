/**
 * One day at a glance: what was written about it, how the hours went, and where the
 * time went.
 *
 * The summary buttons are disabled with the backend's own reason next to them rather
 * than hidden. A person who has not set up a model should be able to see that the
 * feature exists and what it needs, not wonder where it went.
 */
import { useCallback, useEffect, useState } from "react";
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
import { duration } from "../lib/format";

interface Props {
  date: string;
  onChangeDate: (date: string) => void;
  revision: number;
}

/** A date `days` away from the given one, in the same YYYY-MM-DD form. */
function shift(date: string, days: number): string {
  const [year, month, day] = date.split("-").map(Number);
  const moved = new Date(year ?? 1970, (month ?? 1) - 1, (day ?? 1) + days);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${moved.getFullYear()}-${pad(moved.getMonth() + 1)}-${pad(moved.getDate())}`;
}

function hourLabel(hour: number): string {
  return `${String(hour).padStart(2, "0")}:00`;
}

export default function DayView({ date, onChangeDate, revision }: Props) {
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

  return (
    <section aria-label="Day view">
      <div className="section__head">
        <h2 className="section__title">{date === today ? `Today · ${date}` : date}</h2>
        <div className="daynav">
          <button
            type="button"
            className="button button--quiet"
            onClick={() => onChangeDate(shift(date, -1))}
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
            onClick={() => onChangeDate(shift(date, 1))}
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
                <li key={hour.hour} className="hourbar">
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
                    {said ? (
                      said.text
                    ) : (
                      <button
                        type="button"
                        className="episode__more"
                        disabled={!ready || busy !== null}
                        onClick={() =>
                          run(`hour-${hour.hour}`, () => summarizeHour(date, hour.hour))
                        }
                      >
                        {busy === `hour-${hour.hour}` ? "Writing…" : "Summarize this hour"}
                      </button>
                    )}
                  </span>
                </li>
              );
            })}
          </ol>
        )}
      </section>

      <section className="panel" aria-label="Applications">
        <h3 className="panel__title">Where the time went</h3>
        {report && report.rollup.apps.length > 0 ? (
          <ol className="apps">
            {report.rollup.apps.map((app) => (
              <li key={app.app} className="app">
                <span className="app__name">{app.app}</span>
                <span className="app__bar">
                  <span
                    className="app__fill"
                    style={{
                      width: `${
                        report.rollup.activeMs > 0
                          ? Math.round((app.activeMs / report.rollup.activeMs) * 100)
                          : 0
                      }%`,
                    }}
                  />
                </span>
                <span className="app__active">{duration(app.activeMs)}</span>
              </li>
            ))}
          </ol>
        ) : (
          <p className="empty">Nothing was recorded on this day.</p>
        )}
      </section>
    </section>
  );
}
