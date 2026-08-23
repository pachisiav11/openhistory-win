/** Builders shared by the view tests. One shape per thing the backend serializes. */
import {
  localDate,
  mockCommand,
  type AppInfo,
  type AppUsage,
  type CloudModel,
  type Config,
  type DayReport,
  type DaySummary,
  type Episode,
  type HourlyRollup,
  type KeyStatus,
  type LocalModel,
  type McpStatus,
  type Readiness,
  type SearchHit,
  type Status,
} from "../lib/ipc";

export const DATA_DIR = String.raw`C:\Users\you\AppData\Roaming\openhistory-win`;
export const MINUTE = 60_000;

export function status(overrides: Partial<Status> = {}): Status {
  return {
    running: true,
    eventsToday: 2,
    lastEventAt: "2026-08-21T14:05:00.000Z",
    dataDir: DATA_DIR,
    ...overrides,
  };
}

export function episode(overrides: Partial<Episode> = {}): Episode {
  return {
    id: "2026-08-21#1",
    date: "2026-08-21",
    app: "Visual Studio Code",
    title: "collector.rs - openhistory-win",
    start: "2026-08-21T14:05:00.000Z",
    end: "2026-08-21T14:35:00.000Z",
    durationMs: 30 * MINUTE,
    activeMs: 30 * MINUTE,
    eventCount: 4,
    isPrivate: false,
    ...overrides,
  };
}

/** Build the rollup the backend would have derived from these episodes. */
export function report(episodes: Episode[] = [episode()], hours: HourlyRollup[] = []): DayReport {
  const totals = new Map<string, AppUsage>();
  for (const one of episodes) {
    const running = totals.get(one.app) ?? { app: one.app, activeMs: 0, episodes: 0 };
    running.activeMs += one.activeMs;
    running.episodes += 1;
    totals.set(one.app, running);
  }

  return {
    date: localDate(),
    episodes,
    rollup: {
      date: localDate(),
      activeMs: episodes.reduce((sum, one) => sum + one.activeMs, 0),
      idleMs: episodes.reduce((sum, one) => sum + Math.max(0, one.durationMs - one.activeMs), 0),
      episodes: episodes.length,
      apps: [...totals.values()].sort((a, b) => b.activeMs - a.activeMs),
      hours,
      privateEpisodes: episodes.filter((one) => one.isPrivate).length,
    },
  };
}

export function hit(overrides: Partial<SearchHit> = {}): SearchHit {
  return {
    id: "2026-08-21#1",
    date: "2026-08-21",
    app: "Visual Studio Code",
    title: "collector.rs - openhistory-win",
    start: "2026-08-21T14:05:00.000Z",
    end: "2026-08-21T14:35:00.000Z",
    activeMs: 30 * MINUTE,
    isPrivate: false,
    matchedTerms: 1,
    ...overrides,
  };
}

export function config(overrides: Partial<Config> = {}): Config {
  return {
    recordingEnabled: true,
    startOnLaunch: true,
    startWithWindows: true,
    retentionDays: 0,
    recording: { excludedApps: ["1password"], captureUrls: true },
    inference: {
      provider: "disabled",
      cloudConsent: false,
      cloudModel: "claude-haiku-4-5",
      contextSize: 8192,
      idleUnloadSeconds: 600,
      autoSummarize: false,
    },
    mcp: { enabled: false, port: 47123, allowHistory: true },
    ...overrides,
  };
}

export function cloudModel(overrides: Partial<CloudModel> = {}): CloudModel {
  return {
    id: "claude-haiku-4-5",
    name: "Claude Haiku (latest)",
    provider: "anthropic",
    vendor: "Anthropic",
    note: "Fast and inexpensive.",
    supportsEffort: false,
    hasKey: false,
    ...overrides,
  };
}

export function localModel(overrides: Partial<LocalModel> = {}): LocalModel {
  return {
    id: "qwen2.5-3b-instruct-q4",
    name: "Qwen2.5 3B Instruct",
    vendor: "Alibaba",
    parameters: "3B",
    quantization: "Q4_K_M",
    repo: "Qwen/Qwen2.5-3B-Instruct-GGUF",
    file: "qwen2.5-3b-instruct-q4_k_m.gguf",
    approximateBytes: 2_100_000_000,
    recommendedRamBytes: 8_000_000_000,
    note: "Small enough for any machine.",
    installed: false,
    fitsMemory: true,
    ...overrides,
  };
}

export function keys(stored: string[] = []): KeyStatus[] {
  return [
    { provider: "anthropic", label: "Anthropic API key", stored: stored.includes("anthropic") },
    { provider: "openai", label: "OpenAI API key", stored: stored.includes("openai") },
    { provider: "google", label: "Google AI Studio API key", stored: stored.includes("google") },
  ];
}

export function summary(overrides: Partial<DaySummary> = {}): DaySummary {
  return { date: localDate(), hours: [], ...overrides };
}

/**
 * Register a backend that answers everything, so a view only has to override the one
 * command it is about. A test that forgets a command gets a working default rather
 * than a rejected promise it has to read the stack trace to understand.
 */
export function backend(
  overrides: {
    day?: DayReport;
    status?: Status;
    config?: Config;
    readiness?: Readiness;
    mcp?: McpStatus;
  } = {},
) {
  let currentStatus = overrides.status ?? status();
  const day = overrides.day ?? report();

  mockCommand(
    "app_info",
    (): AppInfo => ({ name: "OpenHistory", version: "0.1.0", phase: 6, dataDir: DATA_DIR }),
  );
  mockCommand("get_status", () => currentStatus);
  mockCommand("day_report", () => day);
  mockCommand("search_history", (): SearchHit[] => []);
  mockCommand("get_config", () => overrides.config ?? config());
  mockCommand("set_config", (args) => args?.config as Config);
  mockCommand("recorded_days", () => [localDate()]);
  mockCommand("cloud_models", (): CloudModel[] => [cloudModel()]);
  mockCommand("local_models", (): LocalModel[] => [localModel()]);
  mockCommand("api_keys", () => keys());
  mockCommand(
    "inference_readiness",
    (): Readiness =>
      overrides.readiness ?? { provider: "disabled", ready: false, blockedBy: "No model is chosen." },
  );
  mockCommand("local_server_status", () => ({ running: false, managed: false }));
  mockCommand("day_summary", (args) => summary({ date: String(args?.date ?? localDate()) }));
  mockCommand("mcp_status", (): McpStatus => overrides.mcp ?? { running: false, hasToken: false });
  mockCommand("stop_collector", () => {
    currentStatus = { ...currentStatus, running: false };
    return currentStatus;
  });
  mockCommand("start_collector", () => {
    currentStatus = { ...currentStatus, running: true };
    return currentStatus;
  });
}
