import { useEffect, useState } from "react";
import { invoke, isTauri, type AppInfo } from "./lib/ipc";

const PHASES = [
  { n: 0, name: "Scaffold", detail: "Repository, workspace, continuous integration" },
  { n: 1, name: "Collector", detail: "Win32 hooks, UIAutomation, session events" },
  { n: 2, name: "Shell", detail: "Tauri app writing events to disk" },
  { n: 3, name: "Processing", detail: "Episodes, rollups, search index" },
  { n: 4, name: "Inference", detail: "Anthropic and local llama.cpp summaries" },
  { n: 5, name: "MCP", detail: "Authenticated local query endpoint" },
  { n: 6, name: "Interface", detail: "Timeline, search, day view, settings" },
];

export default function App() {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    invoke<AppInfo>("app_info")
      .then(setInfo)
      .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)));
  }, []);

  const current = info?.phase ?? 0;

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

      <ol className="phases">
        {PHASES.map((p) => {
          const state = p.n < current ? "done" : p.n === current ? "active" : "pending";
          return (
            <li key={p.n} className={`phase phase--${state}`}>
              <span className="phase__n">{p.n}</span>
              <span className="phase__body">
                <span className="phase__name">{p.name}</span>
                <span className="phase__detail">{p.detail}</span>
              </span>
              <span className="phase__state">{state}</span>
            </li>
          );
        })}
      </ol>

      <footer className="shell__foot">
        Recording has not started. Nothing is being collected yet.
      </footer>
    </main>
  );
}
