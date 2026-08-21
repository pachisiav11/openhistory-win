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

/** The local date, formatted the way the backend partitions its logs. */
export function localDate(when: Date = new Date()): string {
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${when.getFullYear()}-${pad(when.getMonth() + 1)}-${pad(when.getDate())}`;
}
