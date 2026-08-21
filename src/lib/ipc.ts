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
}

export interface Config {
  recordingEnabled: boolean;
  startOnLaunch: boolean;
  retentionDays: number;
  recording: RecordingConfig;
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

export interface DailyRollup {
  date: string;
  activeMs: number;
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
