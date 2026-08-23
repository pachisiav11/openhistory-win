/**
 * Thin wrapper over Tauri's `invoke`.
 *
 * The views must run in a plain browser as well as inside the app: that is how the
 * frontend is tested headlessly, without a desktop session. When the Tauri runtime is
 * absent, calls are served by a registered mock instead of throwing.
 */
import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";

export type MockHandler = (args?: Record<string, unknown>) => unknown;

const mocks = new Map<string, MockHandler>();

/** Event the backend pushes whenever the recording status changes. */
export const STATUS_EVENT = "openhistory://status";

/** Event the backend pushes while a local model downloads. */
export const DOWNLOAD_EVENT = "openhistory://download";

/** True when running inside the Tauri WebView rather than a plain browser. */
export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/** Register a stand-in for one IPC command. Used by tests and by browser previews. */
export function mockCommand(command: string, handler: MockHandler): void {
  mocks.set(command, handler);
}

export function clearMocks(): void {
  mocks.clear();
  statusHandlers.clear();
  downloadHandlers.clear();
}

export async function invoke<T>(
  command: string,
  args?: Record<string, unknown>,
): Promise<T> {
  if (isTauri()) {
    return tauriInvoke<T>(command, args);
  }

  const mock = mocks.get(command);
  if (!mock) {
    throw new Error(
      `IPC command "${command}" was called outside Tauri with no mock registered.`,
    );
  }
  return (await mock(args)) as T;
}

type StatusHandler = (status: Status) => void;
const statusHandlers = new Set<StatusHandler>();

/**
 * Subscribe to backend status pushes. Returns an unsubscribe function.
 *
 * Outside Tauri the subscription is served locally, so a test or a browser preview
 * can drive the same code path with {@link emitStatus}.
 */
export function onStatus(handler: StatusHandler): () => void {
  if (!isTauri()) {
    statusHandlers.add(handler);
    return () => statusHandlers.delete(handler);
  }

  let stop: (() => void) | undefined;
  let cancelled = false;
  void tauriListen<Status>(STATUS_EVENT, (event) => handler(event.payload)).then(
    (unlisten) => {
      if (cancelled) unlisten();
      else stop = unlisten;
    },
  );
  return () => {
    cancelled = true;
    stop?.();
  };
}

/** Push a status to every local subscriber. No effect inside Tauri. */
export function emitStatus(status: Status): void {
  for (const handler of statusHandlers) handler(status);
}

export interface AppInfo {
  name: string;
  version: string;
  phase: number;
  dataDir: string;
}

export interface Status {
  running: boolean;
  eventsToday: number;
  lastEventAt: string | null;
  dataDir: string;
}

export interface RecordingConfig {
  excludedApps: string[];
  captureUrls: boolean;
  /** Record the name of the document each window is on. */
  captureDocuments: boolean;
  /** Record a bounded, redacted sample of the text a window is showing. */
  captureVisibleText: boolean;
}

export type InferenceProvider = "disabled" | "anthropic" | "openai" | "google" | "local";

export interface InferenceConfig {
  provider: InferenceProvider;
  /** Nothing is sent to a cloud provider until this is true. */
  cloudConsent: boolean;
  cloudModel: string;
  localModelId?: string;
  localModelPath?: string;
  contextSize: number;
  idleUnloadSeconds: number;
  autoSummarize: boolean;
}

export interface McpConfig {
  enabled: boolean;
  port: number;
  allowHistory: boolean;
}

export interface Config {
  recordingEnabled: boolean;
  startOnLaunch: boolean;
  /** Whether Windows launches the application at sign-in. */
  startWithWindows: boolean;
  retentionDays: number;
  recording: RecordingConfig;
  inference: InferenceConfig;
  mcp: McpConfig;
}

export interface ApplicationDescriptor {
  name: string;
  path: string;
  pid: number;
  bundleId?: string;
}

export interface BrowserObservation {
  url?: string;
  isPrivate: boolean;
}

/**
 * One recorded observation. Optional fields are absent rather than null, matching the
 * on-disk schema exactly.
 */
export interface ActivityEvent {
  version: number;
  id: string;
  timestamp: string;
  kind: string;
  application?: ApplicationDescriptor;
  windowTitle?: string;
  browser?: BrowserObservation;
  isSensitive?: boolean;
}

/**
 * A stretch of continuous work in one application.
 *
 * `durationMs` is wall-clock from first event to last; `activeMs` is the part of it
 * there is evidence for. Measurements use `activeMs`.
 */
export interface Episode {
  id: string;
  date: string;
  app: string;
  appPath?: string;
  title?: string;
  titles?: string[];
  urls?: string[];
  /** Documents worked on, by name. The location stays in the event log. */
  documents?: string[];
  /** Lines of interface text, already bounded and redacted when recorded. */
  visibleText?: string[];
  start: string;
  end: string;
  durationMs: number;
  activeMs: number;
  eventCount: number;
  isPrivate: boolean;
}

export interface AppUsage {
  app: string;
  activeMs: number;
  episodes: number;
}

export interface HourlyRollup {
  hour: number;
  activeMs: number;
  apps: AppUsage[];
  episodeIds: string[];
}

/**
 * `activeMs` is time there is evidence for; `idleMs` is the rest of the time the day's
 * episodes span, when a window sat in front and nothing happened in it. Together they
 * are the day's screen time. Locked and sleeping stretches are in neither.
 */
export interface DailyRollup {
  date: string;
  activeMs: number;
  idleMs: number;
  episodes: number;
  apps: AppUsage[];
  hours: HourlyRollup[];
  firstActivity?: string;
  lastActivity?: string;
  privateEpisodes: number;
}

export interface DayReport {
  date: string;
  episodes: Episode[];
  rollup: DailyRollup;
}

/** One episode matched by a search, with how many query terms it matched. */
export interface SearchHit {
  id: string;
  date: string;
  app: string;
  title?: string;
  start: string;
  end: string;
  activeMs: number;
  isPrivate: boolean;
  matchedTerms: number;
}

/** Episodes and measurements for one local day, processed on demand. */
export function dayReport(date: string = localDate()): Promise<DayReport> {
  return invoke<DayReport>("day_report", { date });
}

/** Episodes matching every term in the query, most recent first. */
export function searchHistory(query: string, limit = 50): Promise<SearchHit[]> {
  return invoke<SearchHit[]>("search_history", { query, limit });
}

/** Discard everything derived and rebuild it from the event log. */
export function rebuildHistory(): Promise<string[]> {
  return invoke<string[]>("rebuild_history");
}

/** The local date, formatted the way the backend partitions its logs. */
export function localDate(when: Date = new Date()): string {
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${when.getFullYear()}-${pad(when.getMonth() + 1)}-${pad(when.getDate())}`;
}

/* ── Summaries, models and keys ─────────────────────────────────────────────── */

/** One entry of the cloud dropdown. Seven models across three providers. */
export interface CloudModel {
  id: string;
  name: string;
  provider: InferenceProvider;
  note: string;
  supportsEffort: boolean;
  /** The company that runs it, used to group the list. */
  vendor: string;
  /** A key for this provider is stored, so choosing this model would work. */
  hasKey: boolean;
}

/** Whether summaries can be produced with the current settings. */
export interface Readiness {
  provider: string;
  ready: boolean;
  blockedBy?: string;
  model?: string;
}

/** A downloadable model, with what is true of it on this machine. */
export interface LocalModel {
  id: string;
  name: string;
  vendor: string;
  parameters: string;
  quantization: string;
  repo: string;
  file: string;
  approximateBytes: number;
  recommendedRamBytes: number;
  note: string;
  installed: boolean;
  installedBytes?: number;
  path?: string;
  fitsMemory: boolean;
}

/** One step of a download. Arrives on {@link DOWNLOAD_EVENT}. */
export interface DownloadProgress {
  modelId: string;
  downloadedBytes: number;
  totalBytes?: number;
  done: boolean;
  /** Set when the download stopped without finishing. Cancelling counts. */
  error?: string;
}

/** Which providers have a key stored. The key itself never comes back. */
export interface KeyStatus {
  provider: InferenceProvider;
  label: string;
  stored: boolean;
}

export interface LlamaStatus {
  running: boolean;
  port?: number;
  model?: string;
  managed: boolean;
  idleSeconds?: number;
}

export interface HourSummary {
  hour: number;
  text: string;
  activeMs: number;
  generatedAt: string;
  provider: string;
  model: string;
}

export interface DaySummary {
  date: string;
  daily?: string;
  dailyGeneratedAt?: string;
  hours: HourSummary[];
}

/** What one summarization run did. */
export interface RunReport {
  date: string;
  hoursWritten: number[];
  hoursSkipped: number[];
  hoursTooQuiet: number[];
  dailyWritten: boolean;
  failure?: string;
}

export const cloudModels = () => invoke<CloudModel[]>("cloud_models");
export const useCloudModel = (id: string) => invoke<Config>("use_cloud_model", { id });
export const inferenceReadiness = () => invoke<Readiness>("inference_readiness");

export const localModels = () => invoke<LocalModel[]>("local_models");
export const downloadModel = (id: string) => invoke<LocalModel>("download_model", { id });
export const cancelDownload = (id: string) => invoke<void>("cancel_download", { id });
export const removeModel = (id: string) => invoke<LocalModel>("remove_model", { id });
export const useLocalModel = (id: string) => invoke<Config>("use_local_model", { id });

export const apiKeys = () => invoke<KeyStatus[]>("api_keys");
export const storeApiKey = (provider: InferenceProvider, key: string) =>
  invoke<boolean>("store_api_key", { provider, key });
export const forgetApiKey = (provider: InferenceProvider) =>
  invoke<void>("forget_api_key", { provider });

export const daySummary = (date: string) => invoke<DaySummary>("day_summary", { date });
export const summarizeDay = (date: string, rewrite = false) =>
  invoke<RunReport>("summarize_day", { date, rewrite });
export const summarizeHour = (date: string, hour: number) =>
  invoke<HourSummary>("summarize_hour", { date, hour });
export const forgetSummary = (date: string) => invoke<void>("forget_summary", { date });

export const localServerStatus = () => invoke<LlamaStatus>("local_server_status");
export const stopLocalServer = () => invoke<LlamaStatus>("stop_local_server");

/* ── The local MCP server ───────────────────────────────────────────────────── */

export interface McpStatus {
  running: boolean;
  port?: number;
  url?: string;
  /** A token exists, so a client could authenticate. */
  hasToken: boolean;
}

/** A status, plus a token when one was minted just now. */
export interface McpHandle extends McpStatus {
  token?: string;
}

export const mcpStatus = () => invoke<McpStatus>("mcp_status");
export const startMcp = () => invoke<McpHandle>("start_mcp");
export const stopMcp = () => invoke<McpStatus>("stop_mcp");
export const regenerateMcpToken = () => invoke<string>("regenerate_mcp_token");
export const forgetMcpTokens = () => invoke<McpStatus>("forget_mcp_tokens");
export const mcpClientConfig = (token?: string) =>
  invoke<string>("mcp_client_config", token ? { token } : {});

/* ── The summary library ────────────────────────────────────────────────────── */

/** One saved summary, described without reading its body. */
export interface LibraryEntry {
  id: string;
  title: string;
  /** The local day the document describes. */
  date: string;
  savedAt: string;
  bytes: number;
}

export const libraryEntries = () => invoke<LibraryEntry[]>("library_entries");
export const libraryDocument = (id: string) => invoke<string>("library_document", { id });
export const librarySave = (date: string) => invoke<LibraryEntry>("library_save", { date });
export const libraryDelete = (id: string) => invoke<void>("library_delete", { id });

/** Write a copy wherever the user chooses. Null when they dismissed the dialog. */
export const libraryExport = (id: string) => invoke<string | null>("library_export", { id });

/* ── Settings and data ──────────────────────────────────────────────────────── */

export const getConfig = () => invoke<Config>("get_config");
export const setConfig = (config: Config) => invoke<Config>("set_config", { config });
export const recordedDays = () => invoke<string[]>("recorded_days");

/** What a delete-everything left behind: nothing, and this says how much of it. */
export interface Deleted {
  days: number;
  summaries: number;
}

export const deleteAllHistory = () => invoke<Deleted>("delete_all_history");

type DownloadHandler = (progress: DownloadProgress) => void;
const downloadHandlers = new Set<DownloadHandler>();

/**
 * Subscribe to download progress. Returns an unsubscribe function.
 *
 * Served locally outside Tauri, the same way {@link onStatus} is, so the settings
 * page can be driven in a browser with {@link emitDownload}.
 */
export function onDownload(handler: DownloadHandler): () => void {
  if (!isTauri()) {
    downloadHandlers.add(handler);
    return () => downloadHandlers.delete(handler);
  }

  let stop: (() => void) | undefined;
  let cancelled = false;
  void tauriListen<DownloadProgress>(DOWNLOAD_EVENT, (event) =>
    handler(event.payload),
  ).then((unlisten) => {
    if (cancelled) unlisten();
    else stop = unlisten;
  });
  return () => {
    cancelled = true;
    stop?.();
  };
}

/** Push a download step to every local subscriber. No effect inside Tauri. */
export function emitDownload(progress: DownloadProgress): void {
  for (const handler of downloadHandlers) handler(progress);
}
