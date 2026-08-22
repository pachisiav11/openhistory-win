import { useCallback, useEffect, useRef, useState } from "react";
import { invoke, isTauri, localDate, onStatus, type AppInfo, type Status } from "./lib/ipc";
import { clockTime } from "./lib/format";
import Timeline from "./views/Timeline";
import Search from "./views/Search";
import DayView from "./views/DayView";
import Settings from "./views/Settings";

/** How long to wait after the last recorded event before reprocessing the day. */
const REFRESH_DELAY_MS = 800;

const VIEWS = [
  { id: "timeline", label: "Timeline" },
  { id: "search", label: "Search" },
  { id: "day", label: "Day" },
  { id: "settings", label: "Settings" },
] as const;

type ViewId = (typeof VIEWS)[number]["id"];

export default function App() {
  const [view, setView] = useState<ViewId>("timeline");
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [status, setStatus] = useState<Status | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // Bumped whenever the recorded history may have moved. The views watch it rather
  // than each subscribing to the status stream and each debouncing it separately.
  const [revision, setRevision] = useState(0);
  const [day, setDay] = useState(localDate());

  const fail = useCallback((cause: unknown) => {
    setError(cause instanceof Error ? cause.message : String(cause));
  }, []);

  useEffect(() => {
    invoke<AppInfo>("app_info").then(setInfo).catch(fail);
    invoke<Status>("get_status").then(setStatus).catch(fail);
  }, [fail]);

  // The backend pushes a status on every recorded event. Reprocessing the day on each
  // one would rescan the whole log during a burst of window switching, so the refresh
  // trails the last event instead.
  const pending = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => {
    const stop = onStatus((pushed) => {
      setStatus(pushed);
      clearTimeout(pending.current);
      pending.current = setTimeout(() => setRevision((n) => n + 1), REFRESH_DELAY_MS);
    });
    return () => {
      clearTimeout(pending.current);
      stop();
    };
  }, []);

  const toggleRecording = useCallback(() => {
    const command = status?.running ? "stop_collector" : "start_collector";
    setBusy(true);
    setError(null);
    invoke<Status>(command)
      .then((next) => {
        setStatus(next);
        setRevision((n) => n + 1);
      })
      .catch(fail)
      .finally(() => setBusy(false));
  }, [status?.running, fail]);

  const openDay = useCallback((date: string) => {
    setDay(date);
    setView("day");
  }, []);

  const running = status?.running ?? false;

  return (
    <div className="shell">
      <header className="shell__head">
        <div>
          <h1 className="shell__title">OpenHistory</h1>
          <p className="shell__sub">
            Local-first activity history for Windows
            {info ? ` · v${info.version}` : ""}
            {isTauri() ? "" : " · browser preview"}
          </p>
        </div>

        <section className="status" aria-label="Recording status">
          <span
            className={`status__dot status__dot--${running ? "on" : "off"}`}
            aria-hidden="true"
          />
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
      </header>

      <nav className="tabs" aria-label="Views">
        {VIEWS.map((one) => (
          <button
            key={one.id}
            type="button"
            className={`tab${view === one.id ? " tab--current" : ""}`}
            aria-current={view === one.id ? "page" : undefined}
            onClick={() => setView(one.id)}
          >
            {one.label}
          </button>
        ))}
      </nav>

      {error ? (
        <p className="notice notice--error" role="alert">
          {error}
        </p>
      ) : null}

      <main className="view">
        {view === "timeline" ? <Timeline revision={revision} onOpenDay={openDay} /> : null}
        {view === "search" ? <Search onOpenDay={openDay} /> : null}
        {view === "day" ? <DayView date={day} onChangeDate={setDay} revision={revision} /> : null}
        {view === "settings" ? <Settings onChanged={() => setRevision((n) => n + 1)} /> : null}
      </main>

      <footer className="shell__foot">
        {info?.dataDir ? `History is kept in ${info.dataDir}` : "History is kept on this machine."}
      </footer>
    </div>
  );
}
