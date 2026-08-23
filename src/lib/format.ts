/** Formatting shared by the views. Mirrors what the backend prints in its logs. */

/** A duration in milliseconds as `2h 15m`, `15m`, or `40s`. */
export function duration(ms: number): string {
  const seconds = Math.max(0, Math.round(ms / 1000));
  if (seconds < 60) return `${seconds}s`;

  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m`;

  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  return rest === 0 ? `${hours}h` : `${hours}h ${rest}m`;
}

/**
 * The local hour a timestamp falls in, or undefined if it cannot be read.
 *
 * Rollups file an episode under the local hour, so a timestamp has to be converted the
 * same way before it can be matched against one.
 */
export function localHour(timestamp: string): number | undefined {
  const when = new Date(timestamp);
  return Number.isNaN(when.getTime()) ? undefined : when.getHours();
}

/** A timestamp as local `HH:MM`, or `--:--` if it cannot be read. */
export function clockTime(timestamp: string): string {
  const when = new Date(timestamp);
  if (Number.isNaN(when.getTime())) return "--:--";
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(when.getHours())}:${pad(when.getMinutes())}`;
}

/** A date `days` away from the given one, in the same YYYY-MM-DD form. */
export function shiftDate(date: string, days: number): string {
  const [year, month, day] = date.split("-").map(Number);
  const moved = new Date(year ?? 1970, (month ?? 1) - 1, (day ?? 1) + days);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${moved.getFullYear()}-${pad(moved.getMonth() + 1)}-${pad(moved.getDate())}`;
}

/** A size in bytes as `820 bytes` or `2.1 kB`. */
export function fileSize(bytes: number): string {
  if (bytes < 1000) return `${Math.max(0, Math.round(bytes))} bytes`;
  return `${(bytes / 1000).toFixed(1)} kB`;
}
