/**
 * Fixtures that let the whole frontend run in an ordinary browser tab.
 *
 * This is what makes the UI verifiable without a desktop session: `npm run dev`
 * opened outside Tauri serves representative data instead of failing on every IPC
 * call. Inside the real app these registrations are never consulted.
 */
import { isTauri, mockCommand, type AppInfo } from "./ipc";

export function installBrowserMocks(): void {
  if (isTauri()) return;

  mockCommand("app_info", (): AppInfo => ({
    name: "OpenHistory",
    version: "0.1.0",
    phase: 0,
  }));
}
