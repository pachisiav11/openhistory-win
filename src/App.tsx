import { useCallback, useEffect, useRef, useState } from "react";
import {
  dayReport,
  invoke,
  isTauri,
  onStatus,
  type AppInfo,
  type DayReport,
  type Episode,
  type Status,
} from "./lib/ipc";
import { clockTime, duration } from "./lib/format";

/** How long to wait after the last recorded event before reprocessing the day. */
const REFRESH_DELAY_MS = 800;

function summarise(report: DayReport): string {
  const { rollup } = report;
  const parts = [`${duration(rollup.activeMs)} active`];
  parts.push(rollup.episodes === 1 ? "1 episode" : `${rollup.episodes} episodes`);

  const leader = rollup.apps[0];
  if (leader && rollup.apps.length > 1) {
    parts.push(`mostly ${leader.app}`);
  }
  if (rollup.privateEpisodes > 0) {
    parts.push(
      rollup.privateEpisodes === 1 ? "1 private session" : `${rollup.privateEpisodes} private sessions`,
    );
  }
  return parts.join(" · ");
}

function detailOf(episode: Episode): string {
  if (episode.isPrivate) return "Private browsing — nothing was recorded";
  return episode.title ?? "";
}

export default function App() {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [status, setStatus] = useState<Status | null>(null);
  const [report, setReport] = useState<DayReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const fail = useCallback((cause: unknown) => {
    setError(cause instanceof Error ? cause.message : String(cause));
  }, []);

  const loadToday = useCallback(() => {
    dayReport().then(setReport).catch(fail);
  }, [fail]);

  useEffect(() => {
    invoke<AppInfo>("app_info").then(setInfo).catch(fail);
    invoke<Status>("get_status").then(setStatus).catch(fail);
    loadToday();
  }, [loadToday, fail]);

  // The backend pushes a status on every recorded event. Reprocessing the day on
  // each one would rescan the whole log during a burst of window switching, so the
  // refresh trails the last event instead.
  const pending = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => {
    const stop = onStatus((pushed) => {
      setStatus(pushed);
      clearTimeout(pending.current);
      pending.current = setTimeout(loadToday, REFRESH_DELAY_MS);
    });
    return () => {
      clearTimeout(pending.current);
      stop();
    };
  }, [loadToday]);

  const toggleRecording = useCallback(() => {
    const command = status?.running ? "stop_collector" : "start_collector";
    setBusy(true);
    setError(null);
    invoke<Status>(command)
      .then((next) => {
        setStatus(next);
        loadToday();
      })
      .catch(fail)
      .finally(() => setBusy(false));
  }, [status?.running, loadToday, fail]);

  const running = status?.running ?? false;
  const episodes = report ? [...report.episodes].reverse() : [];

  return (
    <main className="shell">
      <header className="shell__head">
        <h1 className="shell__title">OpenHistory</h1>
        <p className="shell__sub">
          Local-first activity history for Windows
          {info ? ` · v${info.version}` : ""}
          {isTauri() ? "" : " · browser preview"}
        </p>
      </header>

      {error ? (
        <p className="notice notice--error" role="alert">
          {error}
        </p>
      ) : null}

      <section className="status" aria-label="Recording status">
        <span className={`status__dot status__dot--${running ? "on" : "off"}`} aria-hidden="true" />
        <div className="status__body">
          <p className="status__state">{running ? "Recording" : "Paused"}</p>
          <p className="status__detail">
            {status ? `${status.eventsToday} events today` : "Checking…"}
            {status?.lastEventAt ? ` · last at ${clockTime(status.lastEventAt)}` : ""}
          </p>
        </div>
        <button
          type="button"
          className="button"
          onClick={toggleRecording}
          disabled={busy}
          aria-pressed={running}
        >
          {running ? "Pause" : "Resume"}
        </button>
      </section>

      <section aria-label="Today">
        <h2 className="section__title">Today</h2>
        {report && report.episodes.length > 0 ? (
          <p className="summary">{summarise(report)}</p>
        ) : null}

        {episodes.length === 0 ? (
          <p className="empty">
            Nothing recorded yet today. Switch to another window and it will appear here.
          </p>
        ) : (
          <ol className="episodes">
            {episodes.map((episode) => (
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
                  <span className="episode__detail">{detailOf(episode)}</span>
                </span>
              </li>
            ))}
          </ol>
        )}
      </section>

      <footer className="shell__foot">
        {info?.dataDir ? `History is kept in ${info.dataDir}` : "History is kept on this machine."}
      </footer>
    </main>
  );
}
