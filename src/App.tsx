import { useCallback, useEffect, useState } from "react";
import {
  invoke,
  isTauri,
  localDate,
  onStatus,
  type ActivityEvent,
  type AppInfo,
  type Status,
} from "./lib/ipc";

/** Human labels for the event kinds the Windows collector produces. */
const KIND_LABELS: Record<string, string> = {
  collectorStarted: "Recording started",
  applicationActivated: "Switched to",
  windowChanged: "Window",
  urlChanged: "Opened",
  applicationTerminated: "Closed",
  screenSlept: "Screen off",
  screenWoke: "Screen on",
  sessionLocked: "Locked",
  sessionUnlocked: "Unlocked",
  privacyBoundary: "Private browsing",
};

function clockTime(timestamp: string): string {
  const when = new Date(timestamp);
  if (Number.isNaN(when.getTime())) return "--:--";
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(when.getHours())}:${pad(when.getMinutes())}`;
}

function describe(event: ActivityEvent): string {
  if (event.kind === "privacyBoundary") {
    return "Nothing was recorded";
  }
  return event.browser?.url ?? event.windowTitle ?? event.application?.name ?? "";
}

export default function App() {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [status, setStatus] = useState<Status | null>(null);
  const [events, setEvents] = useState<ActivityEvent[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const report = useCallback((cause: unknown) => {
    setError(cause instanceof Error ? cause.message : String(cause));
  }, []);

  const loadToday = useCallback(() => {
    invoke<ActivityEvent[]>("read_day", { date: localDate() })
      .then(setEvents)
      .catch(report);
  }, [report]);

  useEffect(() => {
    invoke<AppInfo>("app_info").then(setInfo).catch(report);
    invoke<Status>("get_status").then(setStatus).catch(report);
    loadToday();
  }, [loadToday, report]);

  // The backend pushes a status on every recorded event, which is also the signal
  // that the day's log has grown.
  useEffect(() => {
    return onStatus((pushed) => {
      setStatus(pushed);
      loadToday();
    });
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
      .catch(report)
      .finally(() => setBusy(false));
  }, [status?.running, loadToday, report]);

  const running = status?.running ?? false;

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
        {events.length === 0 ? (
          <p className="empty">
            Nothing recorded yet today. Switch to another window and it will appear here.
          </p>
        ) : (
          <ol className="events">
            {[...events].reverse().map((event) => (
              <li key={event.id} className={`event event--${event.kind}`}>
                <time className="event__time" dateTime={event.timestamp}>
                  {clockTime(event.timestamp)}
                </time>
                <span className="event__body">
                  <span className="event__kind">
                    {KIND_LABELS[event.kind] ?? event.kind}
                    {event.application ? ` ${event.application.name}` : ""}
                  </span>
                  <span className="event__detail">{describe(event)}</span>
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
