/**
 * Fixtures that let the whole frontend run in an ordinary browser tab.
 *
 * This is what makes the UI verifiable without a desktop session: `npm run dev`
 * opened outside Tauri serves representative data instead of failing on every IPC
 * call. Inside the real app these registrations are never consulted.
 */
import {
  emitStatus,
  isTauri,
  mockCommand,
  type ActivityEvent,
  type AppInfo,
  type Config,
  type Status,
} from "./ipc";

const DATA_DIR = String.raw`C:\Users\you\AppData\Roaming\openhistory-win`;

function minutesAgo(minutes: number): string {
  return new Date(Date.now() - minutes * 60_000).toISOString();
}

function sample(): ActivityEvent[] {
  return [
    {
      version: 1,
      id: "1",
      timestamp: minutesAgo(46),
      kind: "collectorStarted",
    },
    {
      version: 1,
      id: "2",
      timestamp: minutesAgo(45),
      kind: "applicationActivated",
      application: { name: "Visual Studio Code", path: String.raw`C:\vscode\Code.exe`, pid: 4242 },
      windowTitle: "collector.rs - openhistory-win",
    },
    {
      version: 1,
      id: "3",
      timestamp: minutesAgo(21),
      kind: "urlChanged",
      application: { name: "Google Chrome", path: String.raw`C:\chrome\chrome.exe`, pid: 7788 },
      windowTitle: "Win32 accessibility - Google Chrome",
      browser: { url: "https://learn.microsoft.com/windows/win32/winauto/", isPrivate: false },
    },
    {
      version: 1,
      id: "4",
      timestamp: minutesAgo(12),
      kind: "privacyBoundary",
      application: { name: "Google Chrome", path: String.raw`C:\chrome\chrome.exe`, pid: 7788 },
      browser: { isPrivate: true },
    },
    {
      version: 1,
      id: "5",
      timestamp: minutesAgo(3),
      kind: "applicationActivated",
      application: { name: "Slack", path: String.raw`C:\slack\slack.exe`, pid: 9001 },
      windowTitle: "#engineering - Slack",
    },
  ];
}

export function installBrowserMocks(): void {
  if (isTauri()) return;

  const events = sample();
  let config: Config = {
    recordingEnabled: true,
    startOnLaunch: true,
    retentionDays: 0,
    recording: { excludedApps: ["1password", "bitwarden", "keepassxc"], captureUrls: true },
  };
  let status: Status = {
    running: true,
    eventsToday: events.length,
    lastEventAt: events.at(-1)?.timestamp ?? null,
    dataDir: DATA_DIR,
  };

  mockCommand("app_info", (): AppInfo => ({
    name: "OpenHistory",
    version: "0.1.0",
    phase: 2,
    dataDir: DATA_DIR,
  }));

  mockCommand("get_status", (): Status => status);
  mockCommand("get_config", (): Config => config);

  mockCommand("set_config", (args): Config => {
    config = args?.config as Config;
    status = { ...status, running: config.recordingEnabled };
    emitStatus(status);
    return config;
  });

  mockCommand("start_collector", (): Status => {
    config = { ...config, recordingEnabled: true };
    status = { ...status, running: true };
    emitStatus(status);
    return status;
  });

  mockCommand("stop_collector", (): Status => {
    config = { ...config, recordingEnabled: false };
    status = { ...status, running: false };
    emitStatus(status);
    return status;
  });

  mockCommand("read_day", (args): ActivityEvent[] =>
    args?.date === localToday() ? events : [],
  );

  mockCommand("recorded_days", (): string[] => [localToday()]);
}

function localToday(): string {
  const now = new Date();
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
}
