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
