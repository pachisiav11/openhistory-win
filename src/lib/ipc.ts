/**
 * Thin wrapper over Tauri's `invoke`.
 *
 * The views must run in a plain browser as well as inside the app: that is how the
 * frontend is tested headlessly, without a desktop session. When the Tauri runtime is
 * absent, calls are served by a registered mock instead of throwing.
 */
import { invoke as tauriInvoke } from "@tauri-apps/api/core";

export type MockHandler = (args?: Record<string, unknown>) => unknown;

const mocks = new Map<string, MockHandler>();

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

export interface AppInfo {
  name: string;
  version: string;
  phase: number;
}
