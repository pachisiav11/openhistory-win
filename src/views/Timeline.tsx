/**
 * Today, grouped by the hour it happened in.
 *
 * An episode is placed in the hour it started. A session that runs across an hour
 * boundary is one episode in one group rather than two halves, because it was one
 * stretch of work and splitting it would invent a break that never happened. The
 * measured time in the hour headings comes from the backend's rollup, which does
 * apportion across boundaries, so the two never have to agree by coincidence.
 */
import { useCallback, useEffect, useState } from "react";
import { dayReport, localDate, type DayReport, type Episode } from "../lib/ipc";
import { clockTime, duration } from "../lib/format";

interface Props {
  /** Bumped by the shell when the recorded history may have moved. */
  revision: number;
  onOpenDay: (date: string) => void;
}

interface Group {
  hour: number;
  activeMs: number;
  episodes: Episode[];
}

function groupByHour(report: DayReport): Group[] {
  const byHour = new Map<number, Episode[]>();
  for (const episode of report.episodes) {
    const hour = new Date(episode.start).getHours();
    const existing = byHour.get(hour);
    if (existing) existing.push(episode);
    else byHour.set(hour, [episode]);
  }

  const measured = new Map(report.rollup.hours.map((one) => [one.hour, one.activeMs]));
  return [...byHour.entries()]
    .sort((a, b) => b[0] - a[0])
    .map(([hour, episodes]) => ({
      hour,
      activeMs: measured.get(hour) ?? episodes.reduce((sum, one) => sum + one.activeMs, 0),
      episodes: [...episodes].reverse(),
    }));
}

function summarise(report: DayReport): string {
  const { rollup } = report;
  const parts = [`${duration(rollup.activeMs)} active`];
  parts.push(rollup.episodes === 1 ? "1 episode" : `${rollup.episodes} episodes`);

  const leader = rollup.apps[0];
  if (leader && rollup.apps.length > 1) parts.push(`mostly ${leader.app}`);
  if (rollup.privateEpisodes > 0) {
    parts.push(
      rollup.privateEpisodes === 1
        ? "1 private session"
        : `${rollup.privateEpisodes} private sessions`,
    );
  }
  return parts.join(" · ");
}

/** The window titles seen during an episode, without repeating the headline one. */
function otherTitles(episode: Episode): string[] {
  return (episode.titles ?? []).filter((title) => title !== episode.title);
}

function hourLabel(hour: number): string {
  return `${String(hour).padStart(2, "0")}:00`;
}

export default function Timeline({ revision, onOpenDay }: Props) {
  const [report, setReport] = useState<DayReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [expanded, setExpanded] = useState<string | null>(null);

  const today = localDate();

  useEffect(() => {
    let current = true;
    dayReport(today)
      .then((loaded) => {
        if (current) setReport(loaded);
      })
      .catch((cause) => current && setError(String(cause)));
    return () => {
      current = false;
    };
  }, [today, revision]);

  const toggle = useCallback((id: string) => {
    setExpanded((open) => (open === id ? null : id));
  }, []);

  if (error) {
    return (
      <p className="notice notice--error" role="alert">
        {error}
      </p>
    );
  }

  const groups = report ? groupByHour(report) : [];

  return (
    <section aria-label="Timeline">
      <div className="section__head">
        <h2 className="section__title">Today</h2>
        <button type="button" className="button button--quiet" onClick={() => onOpenDay(today)}>
          Open day view
        </button>
      </div>

      {report && report.episodes.length > 0 ? (
        <p className="summary">{summarise(report)}</p>
      ) : null}

      {groups.length === 0 ? (
        <p className="empty">
          Nothing recorded yet today. Switch to another window and it will appear here.
        </p>
      ) : (
        groups.map((group) => (
          <section key={group.hour} className="hour" aria-label={hourLabel(group.hour)}>
            <h3 className="hour__head">
              <span className="hour__label">{hourLabel(group.hour)}</span>
              <span className="hour__active">{duration(group.activeMs)}</span>
            </h3>

            <ol className="episodes">
              {group.episodes.map((episode) => {
                const extra = otherTitles(episode);
                const open = expanded === episode.id;
                return (
                  <li
                    key={episode.id}
                    className={`episode${episode.isPrivate ? " episode--private" : ""}`}
                  >
                    <span className="episode__when">
                      <time dateTime={episode.start}>{clockTime(episode.start)}</time>
                      <span className="episode__length">{duration(episode.activeMs)}</span>
                    </span>

                    <span className="episode__body">
                      <span className="episode__app">{episode.app}</span>
                      <span className="episode__detail">
                        {episode.isPrivate
                          ? "Private browsing — nothing was recorded"
                          : (episode.title ?? "")}
                      </span>

                      {extra.length > 0 ? (
                        <>
                          <button
                            type="button"
                            className="episode__more"
                            aria-expanded={open}
                            onClick={() => toggle(episode.id)}
                          >
                            {open
                              ? "Hide windows"
                              : `${extra.length} more window${extra.length === 1 ? "" : "s"}`}
                          </button>
                          {open ? (
                            <ul className="episode__windows">
                              {extra.map((title) => (
                                <li key={title}>{title}</li>
                              ))}
                            </ul>
                          ) : null}
                        </>
                      ) : null}
                    </span>
                  </li>
                );
              })}
            </ol>
          </section>
        ))
      )}
    </section>
  );
}
