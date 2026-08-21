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
  type AppUsage,
  type Config,
  type DayReport,
  type Episode,
  type SearchHit,
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

/**
 * The episodes the processing layer would derive from {@link sample}.
 *
 * Written out rather than computed: episode detection lives in Rust, and a second
 * implementation here would be a second thing to keep correct.
 */
function sampleEpisodes(): Episode[] {
  return [
    {
      id: `${localToday()}#1`,
      date: localToday(),
      app: "Visual Studio Code",
      appPath: String.raw`C:\vscode\Code.exe`,
      title: "collector.rs - openhistory-win",
      titles: ["collector.rs - openhistory-win", "store.rs - openhistory-win"],
      urls: [],
      start: minutesAgo(45),
      end: minutesAgo(22),
      durationMs: 23 * 60_000,
      activeMs: 23 * 60_000,
      eventCount: 6,
      isPrivate: false,
    },
    {
      id: `${localToday()}#2`,
      date: localToday(),
      app: "Google Chrome",
      appPath: String.raw`C:\chrome\chrome.exe`,
      title: "Win32 accessibility - Google Chrome",
      titles: ["Win32 accessibility - Google Chrome"],
      urls: ["https://learn.microsoft.com/windows/win32/winauto/"],
      start: minutesAgo(21),
      end: minutesAgo(13),
      durationMs: 8 * 60_000,
      activeMs: 8 * 60_000,
      eventCount: 3,
      isPrivate: false,
    },
    {
      id: `${localToday()}#3`,
      date: localToday(),
      app: "Google Chrome",
      appPath: String.raw`C:\chrome\chrome.exe`,
      titles: [],
      urls: [],
      start: minutesAgo(12),
      end: minutesAgo(4),
      durationMs: 8 * 60_000,
      activeMs: 8 * 60_000,
      eventCount: 1,
      isPrivate: true,
    },
    {
      id: `${localToday()}#4`,
      date: localToday(),
      app: "Slack",
      appPath: String.raw`C:\slack\slack.exe`,
      title: "#engineering - Slack",
      titles: ["#engineering - Slack"],
      urls: [],
      start: minutesAgo(3),
      end: minutesAgo(0),
      durationMs: 3 * 60_000,
      activeMs: 3 * 60_000,
      eventCount: 2,
      isPrivate: false,
    },
  ];
}

function sampleReport(): DayReport {
  const episodes = sampleEpisodes();
  const totals = new Map<string, AppUsage>();
  for (const one of episodes) {
    const running = totals.get(one.app) ?? { app: one.app, activeMs: 0, episodes: 0 };
    running.activeMs += one.activeMs;
    running.episodes += 1;
    totals.set(one.app, running);
  }

  const first = episodes[0];
  const last = episodes.at(-1);
  return {
    date: localToday(),
    episodes,
    rollup: {
      date: localToday(),
      activeMs: episodes.reduce((sum, one) => sum + one.activeMs, 0),
      episodes: episodes.length,
      apps: [...totals.values()].sort((a, b) => b.activeMs - a.activeMs),
      hours: [],
      ...(first ? { firstActivity: first.start } : {}),
      ...(last ? { lastActivity: last.end } : {}),
      privateEpisodes: episodes.filter((one) => one.isPrivate).length,
    },
  };
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
    phase: 3,
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

  mockCommand("day_report", (args): DayReport =>
    args?.date === localToday()
      ? sampleReport()
      : {
          date: String(args?.date ?? ""),
          episodes: [],
          rollup: {
            date: String(args?.date ?? ""),
            activeMs: 0,
            episodes: 0,
            apps: [],
            hours: [],
            privateEpisodes: 0,
          },
        },
  );

  mockCommand("search_history", (args): SearchHit[] => {
    const terms = String(args?.query ?? "")
      .toLowerCase()
      .split(/\s+/)
      .filter(Boolean);
    if (terms.length === 0) return [];

    return sampleEpisodes()
      .filter((one) => !one.isPrivate)
      .map((one) => ({
        id: one.id,
        date: one.date,
        app: one.app,
        ...(one.title ? { title: one.title } : {}),
        start: one.start,
        end: one.end,
        activeMs: one.activeMs,
        isPrivate: one.isPrivate,
        matchedTerms: terms.filter((term) =>
          `${one.app} ${one.title ?? ""} ${one.urls?.join(" ") ?? ""}`.toLowerCase().includes(term),
        ).length,
      }))
      .filter((hit) => hit.matchedTerms === terms.length)
      .reverse();
  });

  mockCommand("rebuild_history", (): string[] => [localToday()]);
}

function localToday(): string {
  const now = new Date();
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
}
