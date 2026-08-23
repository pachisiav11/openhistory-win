/**
 * Fixtures that let the whole frontend run in an ordinary browser tab.
 *
 * This is what makes the UI verifiable without a desktop session: `npm run dev`
 * opened outside Tauri serves representative data instead of failing on every IPC
 * call. Inside the real app these registrations are never consulted.
 *
 * The fixtures answer with the same shapes the Rust side serializes, and they keep
 * the same invariants: a private episode carries no title, a stored key is never sent
 * back, and a token can be shown once and then only regenerated.
 */
import {
  emitDownload,
  emitStatus,
  isTauri,
  mockCommand,
  type ActivityEvent,
  type AppInfo,
  type AppUsage,
  type CloudModel,
  type Config,
  type DayReport,
  type DaySummary,
  type Deleted,
  type Episode,
  type HourlyRollup,
  type KeyStatus,
  type LibraryEntry,
  type LlamaStatus,
  type LocalModel,
  type McpHandle,
  type McpStatus,
  type Readiness,
  type RunReport,
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
      documents: ["collector.rs", "store.rs"],
      visibleText: ["Problems", "Terminal", "fn report_window"],
      start: minutesAgo(45),
      end: minutesAgo(22),
      durationMs: 23 * 60_000,
      // Twenty-three minutes in the editor, eight of them with nothing happening, so
      // the preview shows an idle share rather than a day of perfect evidence.
      activeMs: 15 * 60_000,
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
      visibleText: ["UI Automation Overview", "Control Patterns"],
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

  const hours = new Map<number, HourlyRollup>();
  for (const one of episodes) {
    const hour = new Date(one.start).getHours();
    const running = hours.get(hour) ?? { hour, activeMs: 0, apps: [], episodeIds: [] };
    running.activeMs += one.activeMs;
    running.episodeIds.push(one.id);
    hours.set(hour, running);
  }

  const first = episodes[0];
  const last = episodes.at(-1);
  return {
    date: localToday(),
    episodes,
    rollup: {
      date: localToday(),
      activeMs: episodes.reduce((sum, one) => sum + one.activeMs, 0),
      idleMs: episodes.reduce((sum, one) => sum + Math.max(0, one.durationMs - one.activeMs), 0),
      episodes: episodes.length,
      apps: [...totals.values()].sort((a, b) => b.activeMs - a.activeMs),
      hours: [...hours.values()].sort((a, b) => a.hour - b.hour),
      ...(first ? { firstActivity: first.start } : {}),
      ...(last ? { lastActivity: last.end } : {}),
      privateEpisodes: episodes.filter((one) => one.isPrivate).length,
    },
  };
}

function emptyReport(date: string): DayReport {
  return {
    date,
    episodes: [],
    rollup: {
      date,
      activeMs: 0,
      idleMs: 0,
      episodes: 0,
      apps: [],
      hours: [],
      privateEpisodes: 0,
    },
  };
}

/** The seven cloud models the backend offers, in dropdown order. */
function sampleCloudModels(stored: Set<string>): CloudModel[] {
  const entries: Omit<CloudModel, "hasKey">[] = [
    {
      id: "claude-haiku-4-5",
      name: "Claude Haiku (latest)",
      provider: "anthropic",
      vendor: "Anthropic",
      note: "Fast and inexpensive. The default.",
      supportsEffort: false,
    },
    {
      id: "claude-sonnet-5",
      name: "Claude Sonnet (latest)",
      provider: "anthropic",
      vendor: "Anthropic",
      note: "Better writing, a little slower.",
      supportsEffort: true,
    },
    {
      id: "claude-opus-5",
      name: "Claude Opus (latest)",
      provider: "anthropic",
      vendor: "Anthropic",
      note: "The most capable, and the most expensive.",
      supportsEffort: true,
    },
    {
      id: "gpt-5.6-luna",
      name: "GPT-5.6 Luna",
      provider: "openai",
      vendor: "OpenAI",
      note: "Quick and cheap.",
      supportsEffort: true,
    },
    {
      id: "gpt-5.6-terra",
      name: "GPT-5.6 Terra",
      provider: "openai",
      vendor: "OpenAI",
      note: "A balance of speed and quality.",
      supportsEffort: true,
    },
    {
      id: "gpt-5.6-sol",
      name: "GPT-5.6 Sol",
      provider: "openai",
      vendor: "OpenAI",
      note: "The strongest of the three.",
      supportsEffort: true,
    },
    {
      id: "gemini-flash-latest",
      name: "Gemini Flash (latest)",
      provider: "google",
      vendor: "Google AI Studio",
      note: "An alias that follows the newest Flash release.",
      supportsEffort: false,
    },
  ];
  return entries.map((entry) => ({ ...entry, hasKey: stored.has(entry.provider) }));
}

function sampleLocalModels(installed: Set<string>): LocalModel[] {
  const entries = [
    {
      id: "qwen2.5-3b-instruct-q4",
      name: "Qwen2.5 3B Instruct",
      vendor: "Alibaba",
      parameters: "3B",
      quantization: "Q4_K_M",
      repo: "Qwen/Qwen2.5-3B-Instruct-GGUF",
      file: "qwen2.5-3b-instruct-q4_k_m.gguf",
      approximateBytes: 2_100_000_000,
      recommendedRamBytes: 8_000_000_000,
      note: "Small enough for any machine that can run this application.",
    },
    {
      id: "llama-3.1-8b-instruct-q4",
      name: "Llama 3.1 8B Instruct",
      vendor: "Meta",
      parameters: "8B",
      quantization: "Q4_K_M",
      repo: "bartowski/Meta-Llama-3.1-8B-Instruct-GGUF",
      file: "Meta-Llama-3.1-8B-Instruct-Q4_K_M.gguf",
      approximateBytes: 4_900_000_000,
      recommendedRamBytes: 16_000_000_000,
      note: "Writes noticeably better summaries, and wants the memory to match.",
    },
  ];
  return entries.map((entry) => ({
    ...entry,
    installed: installed.has(entry.id),
    fitsMemory: true,
    ...(installed.has(entry.id)
      ? {
          installedBytes: entry.approximateBytes,
          path: `${DATA_DIR}\\models\\${entry.id}`,
        }
      : {}),
  }));
}

export function installBrowserMocks(): void {
  if (isTauri()) return;

  const events = sample();
  let config: Config = {
    recordingEnabled: true,
    startOnLaunch: true,
    startWithWindows: true,
    retentionDays: 0,
    recording: {
      excludedApps: ["1password", "bitwarden", "keepassxc"],
      captureUrls: true,
      captureDocuments: true,
      captureVisibleText: true,
    },
    inference: {
      provider: "disabled",
      cloudConsent: false,
      cloudModel: "claude-haiku-4-5",
      contextSize: 8192,
      idleUnloadSeconds: 600,
      autoSummarize: false,
    },
    mcp: { enabled: false, port: 47123, allowHistory: true },
  };
  let status: Status = {
    running: true,
    eventsToday: events.length,
    lastEventAt: events.at(-1)?.timestamp ?? null,
    dataDir: DATA_DIR,
  };

  const storedKeys = new Set<string>();
  const installedModels = new Set<string>();
  const summaries = new Map<string, DaySummary>();
  let mcp: McpStatus = { running: false, hasToken: false };
  let deleted = false;

  const report = () => (deleted ? emptyReport(localToday()) : sampleReport());
  const episodes = () => (deleted ? [] : sampleEpisodes());

  mockCommand(
    "app_info",
    (): AppInfo => ({
      name: "OpenHistory",
      version: "0.1.0",
      phase: 6,
      dataDir: DATA_DIR,
    }),
  );

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
    args?.date === localToday() && !deleted ? events : [],
  );

  mockCommand("recorded_days", (): string[] => (deleted ? [] : [localToday()]));

  mockCommand("day_report", (args): DayReport => {
    const date = String(args?.date ?? "");
    return date === localToday() ? report() : emptyReport(date);
  });

  mockCommand("search_history", (args): SearchHit[] => {
    const terms = String(args?.query ?? "")
      .toLowerCase()
      .split(/\s+/)
      .filter(Boolean);
    if (terms.length === 0) return [];

    return episodes()
      .map((one) => ({
        id: one.id,
        date: one.date,
        app: one.app,
        // A private episode is indexed by application name only, and its title never
        // comes back with it.
        ...(one.title && !one.isPrivate ? { title: one.title } : {}),
        start: one.start,
        end: one.end,
        activeMs: one.activeMs,
        isPrivate: one.isPrivate,
        matchedTerms: terms.filter((term) =>
          (one.isPrivate ? one.app : `${one.app} ${one.title ?? ""} ${one.urls?.join(" ") ?? ""}`)
            .toLowerCase()
            .includes(term),
        ).length,
      }))
      .filter((hit) => hit.matchedTerms === terms.length)
      .reverse();
  });

  mockCommand("rebuild_history", (): string[] => (deleted ? [] : [localToday()]));

  mockCommand("delete_all_history", (): Deleted => {
    const gone: Deleted = { days: deleted ? 0 : 1, summaries: summaries.size };
    deleted = true;
    summaries.clear();
    return gone;
  });

  /* ── Summaries, models and keys ───────────────────────────────────────────── */

  mockCommand("cloud_models", (): CloudModel[] => sampleCloudModels(storedKeys));

  mockCommand("use_cloud_model", (args): Config => {
    const id = String(args?.id ?? "");
    const model = sampleCloudModels(storedKeys).find((one) => one.id === id);
    if (!model) throw new Error(`${id} is not a model in the list`);
    config = {
      ...config,
      inference: { ...config.inference, provider: model.provider, cloudModel: id },
    };
    return config;
  });

  mockCommand("use_local_model", (args): Config => {
    config = {
      ...config,
      inference: {
        ...config.inference,
        provider: "local",
        localModelId: String(args?.id ?? ""),
      },
    };
    return config;
  });

  mockCommand("inference_readiness", (): Readiness => {
    const { provider, cloudConsent, cloudModel, localModelId } = config.inference;
    if (provider === "disabled") {
      return { provider, ready: false, blockedBy: "No model is chosen." };
    }
    if (provider === "local") {
      return localModelId
        ? { provider, ready: true, model: localModelId }
        : { provider, ready: false, blockedBy: "No local model is downloaded." };
    }
    if (!cloudConsent) {
      return {
        provider,
        ready: false,
        model: cloudModel,
        blockedBy: "Cloud summaries need your agreement before anything is sent.",
      };
    }
    if (!storedKeys.has(provider)) {
      return { provider, ready: false, model: cloudModel, blockedBy: "No API key is stored." };
    }
    return { provider, ready: true, model: cloudModel };
  });

  mockCommand("local_models", (): LocalModel[] => sampleLocalModels(installedModels));

  mockCommand("download_model", (args): LocalModel => {
    const id = String(args?.id ?? "");
    const model = sampleLocalModels(installedModels).find((one) => one.id === id);
    if (!model) throw new Error(`${id} is not a model in the catalog`);

    // Walk the progress out over a few frames so the bar is visible in a browser.
    let sent = 0;
    const step = Math.round(model.approximateBytes / 5);
    const tick = setInterval(() => {
      sent = Math.min(model.approximateBytes, sent + step);
      const done = sent >= model.approximateBytes;
      if (done) {
        clearInterval(tick);
        installedModels.add(id);
      }
      emitDownload({
        modelId: id,
        downloadedBytes: sent,
        totalBytes: model.approximateBytes,
        done,
      });
    }, 400);

    return { ...model, installed: true };
  });

  mockCommand("cancel_download", (args): void => {
    emitDownload({
      modelId: String(args?.id ?? ""),
      downloadedBytes: 0,
      done: true,
      error: "The download was cancelled.",
    });
  });

  mockCommand("remove_model", (args): LocalModel => {
    const id = String(args?.id ?? "");
    installedModels.delete(id);
    const model = sampleLocalModels(installedModels).find((one) => one.id === id);
    if (!model) throw new Error(`${id} is not a model in the catalog`);
    return model;
  });

  mockCommand("api_keys", (): KeyStatus[] => [
    { provider: "anthropic", label: "Anthropic API key", stored: storedKeys.has("anthropic") },
    { provider: "openai", label: "OpenAI API key", stored: storedKeys.has("openai") },
    { provider: "google", label: "Google AI Studio API key", stored: storedKeys.has("google") },
  ]);

  mockCommand("store_api_key", (args): boolean => {
    const provider = String(args?.provider ?? "");
    const key = String(args?.key ?? "");
    if (key.trim() === "") storedKeys.delete(provider);
    else storedKeys.add(provider);
    return storedKeys.has(provider);
  });

  mockCommand("forget_api_key", (args): void => {
    storedKeys.delete(String(args?.provider ?? ""));
  });

  mockCommand("day_summary", (args): DaySummary => {
    const date = String(args?.date ?? "");
    return summaries.get(date) ?? { date, hours: [] };
  });

  mockCommand("summarize_day", (args): RunReport => {
    const date = String(args?.date ?? "");
    const rewrite = Boolean(args?.rewrite);
    const existing = summaries.get(date);
    if (existing && !rewrite) {
      return {
        date,
        hoursWritten: [],
        hoursSkipped: existing.hours.map((one) => one.hour),
        hoursTooQuiet: [],
        dailyWritten: false,
      };
    }

    const written = report().rollup.hours.map((hour) => ({
      hour: hour.hour,
      text: `Worked in ${report().rollup.apps[0]?.app ?? "an application"} for most of the hour.`,
      activeMs: hour.activeMs,
      generatedAt: new Date().toISOString(),
      provider: config.inference.provider,
      model: config.inference.cloudModel,
    }));
    summaries.set(date, {
      date,
      daily: "A morning of Rust, a short read of the Win32 documentation, and a private session.",
      dailyGeneratedAt: new Date().toISOString(),
      hours: written,
    });
    return {
      date,
      hoursWritten: written.map((one) => one.hour),
      hoursSkipped: [],
      hoursTooQuiet: [],
      dailyWritten: true,
    };
  });

  mockCommand("summarize_hour", (args) => {
    const date = String(args?.date ?? "");
    const hour = Number(args?.hour ?? 0);
    const existing = summaries.get(date) ?? { date, hours: [] };
    const written = {
      hour,
      text: "A stretch of work in one application.",
      activeMs: report().rollup.hours.find((one) => one.hour === hour)?.activeMs ?? 0,
      generatedAt: new Date().toISOString(),
      provider: config.inference.provider,
      model: config.inference.cloudModel,
    };
    summaries.set(date, {
      ...existing,
      hours: [...existing.hours.filter((one) => one.hour !== hour), written].sort(
        (a, b) => a.hour - b.hour,
      ),
    });
    return written;
  });

  mockCommand("forget_summary", (args): void => {
    summaries.delete(String(args?.date ?? ""));
  });

  mockCommand(
    "local_server_status",
    (): LlamaStatus => ({ running: false, managed: false }),
  );

  mockCommand("stop_local_server", (): LlamaStatus => ({ running: false, managed: false }));

  /* ── The local MCP server ─────────────────────────────────────────────────── */

  mockCommand("mcp_status", (): McpStatus => mcp);

  mockCommand("start_mcp", (): McpHandle => {
    const fresh = !mcp.hasToken;
    config = { ...config, mcp: { ...config.mcp, enabled: true } };
    mcp = {
      running: true,
      port: config.mcp.port,
      url: `http://127.0.0.1:${config.mcp.port}`,
      hasToken: true,
    };
    return fresh ? { ...mcp, token: sampleToken() } : mcp;
  });

  mockCommand("stop_mcp", (): McpStatus => {
    config = { ...config, mcp: { ...config.mcp, enabled: false } };
    mcp = { running: false, hasToken: mcp.hasToken };
    return mcp;
  });

  mockCommand("regenerate_mcp_token", (): string => {
    mcp = { ...mcp, hasToken: true };
    return sampleToken();
  });

  mockCommand("forget_mcp_tokens", (): McpStatus => {
    mcp = { ...mcp, hasToken: false };
    return mcp;
  });

  // The library is a real store in the app. Here it is an ordinary object, so the
  // preview can save, read, delete and fail to export exactly as the app does.
  const library = new Map<string, { entry: LibraryEntry; body: string }>();

  mockCommand("library_entries", (): LibraryEntry[] =>
    [...library.values()]
      .map((one) => one.entry)
      .sort((a, b) => b.savedAt.localeCompare(a.savedAt)),
  );
  mockCommand("library_document", (args): string => {
    const found = library.get(String(args?.id));
    if (!found) throw new Error("that document is no longer in the library");
    return found.body;
  });
  mockCommand("library_save", (args): LibraryEntry => {
    const date = String(args?.date);
    const report = date === localToday() ? sampleReport() : emptyReport(date);
    const written = summaries.get(date);
    const body = [
      `# ${date}`,
      "",
      written?.daily ?? "No whole-day summary was written.",
      "",
      "## Where the time went",
      "",
      `${Math.round((report.rollup.activeMs + report.rollup.idleMs) / 60_000)}m at the machine.`,
      "",
      ...report.rollup.apps.map((one) => `- ${one.app} — ${Math.round(one.activeMs / 60_000)}m`),
    ].join("\n");

    let id = date;
    for (let n = 2; library.has(id); n += 1) id = `${date}-${n}`;
    const entry: LibraryEntry = {
      id,
      title: date,
      date,
      savedAt: new Date().toISOString(),
      bytes: body.length,
    };
    library.set(id, { entry, body });
    return entry;
  });
  mockCommand("library_delete", (args): void => {
    library.delete(String(args?.id));
  });
  mockCommand("library_export", (): string | null => {
    // A browser tab has no save dialog to open, and pretending otherwise would show
    // a success the app cannot deliver here.
    throw new Error("exporting needs the desktop application");
  });

  mockCommand("mcp_client_config", (args): string => {
    if (!mcp.url) throw new Error("the server is not running, so there is no address to give");
    return JSON.stringify(
      {
        mcpServers: {
          openhistory: {
            type: "http",
            url: `${mcp.url}/mcp`,
            headers: { Authorization: `Bearer ${String(args?.token ?? "YOUR_TOKEN")}` },
          },
        },
      },
      null,
      2,
    );
  });
}

/** A token shaped like the real one: the prefix and 256 bits in hex. */
function sampleToken(): string {
  const bytes = new Uint8Array(32);
  crypto.getRandomValues(bytes);
  return `oh_${[...bytes].map((b) => b.toString(16).padStart(2, "0")).join("")}`;
}

function localToday(): string {
  const now = new Date();
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
}
