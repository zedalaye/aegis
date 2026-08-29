/**
 * Presentation helpers.
 *
 * Formatting only — nothing here reads or decides anything. Paths arrive from
 * Rust already canonical, so these functions never normalize, only display.
 */

/** Matches a path separator on either platform family. */
const SEPARATOR = /[\\/]+/;

/**
 * The last segment of a path — the folder's own name.
 *
 * Used to propose a project name from the folder the user picked. Trailing
 * separators are ignored, and a filesystem or drive root (`/`, `C:\`) has no
 * last segment, so it names itself.
 */
export function folderName(path: string): string {
  const segments = path.split(SEPARATOR).filter((segment) => segment.length > 0);
  return segments.at(-1) ?? path;
}

/**
 * Shortens a long path for a fixed-width badge, keeping both ends.
 *
 * The two informative parts of a path are the root it lives under and the
 * folder it ends at; the middle is what can go. Falls back to the original
 * when eliding would not actually save anything.
 */
export function shortenPath(path: string, maxLength = 48): string {
  if (path.length <= maxLength) {
    return path;
  }

  const segments = path.split(SEPARATOR).filter((segment) => segment.length > 0);
  const separator = path.includes("\\") ? "\\" : "/";
  const first = segments.at(0);
  const last = segments.at(-1);

  if (first === undefined || last === undefined || segments.length < 3) {
    return path;
  }

  const elided = `${first}${separator}…${separator}${last}`;
  return elided.length < path.length ? elided : path;
}

/**
 * An RFC3339 timestamp as a short, local, human-readable string.
 *
 * Timestamps cross the wire in UTC; a user reads them in their own timezone,
 * which is what `toLocaleString` does. An unparseable value is shown verbatim
 * rather than as "Invalid Date" — a wrong-looking timestamp is at least a
 * clue, and this is never load-bearing.
 */
export function formatTimestamp(value: string): string {
  const parsed = new Date(value);
  if (Number.isNaN(parsed.getTime())) {
    return value;
  }
  return parsed.toLocaleString(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  });
}

/** `formatTimestamp`, or a dash for a timestamp that does not exist yet. */
export function formatOptionalTimestamp(value: string | null): string {
  return value === null ? "—" : formatTimestamp(value);
}

/** Unit steps for {@link formatBytes}, largest last. */
const BYTE_UNITS = ["B", "KB", "MB", "GB"] as const;

/**
 * A byte count as a short human-readable string.
 *
 * Powers of 1024, one decimal above the first step, and no decimal on bytes
 * themselves — "1.4 KB" is what a person wants from an audit row, "1434 B" is
 * not. The counts this renders are the audit log's `bytes_in` / `bytes_out`,
 * which are what a call actually carried, so the exact number is never the
 * point; the order of magnitude is.
 */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) {
    return "—";
  }

  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < BYTE_UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }

  const rendered = unit === 0 ? String(Math.round(value)) : value.toFixed(1);
  return `${rendered} ${BYTE_UNITS[unit] ?? "B"}`;
}

/**
 * A millisecond duration as a short human-readable string.
 *
 * Three scales, because a tool call spans all three: a filesystem read is
 * single-digit milliseconds, a model-driven shell command is seconds, and a
 * build is minutes. Sub-second values keep their milliseconds — the difference
 * between 3 ms and 300 ms is the difference between a cached read and a cold
 * one, and rounding both to "0.0 s" would hide it.
 */
export function formatDuration(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) {
    return "—";
  }
  if (ms < 1000) {
    return `${Math.round(ms)} ms`;
  }
  if (ms < 60_000) {
    return `${(ms / 1000).toFixed(1)} s`;
  }

  const totalSeconds = Math.round(ms / 1000);
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  return `${minutes}m ${String(seconds).padStart(2, "0")}s`;
}

/**
 * An RFC3339 timestamp as a local clock time, without the date.
 *
 * For dense lists where every row carries one and the date is nearly always
 * today. Seconds are kept: two tool calls in the same turn are frequently
 * within the same minute, and a column where consecutive rows read identically
 * says nothing about their order. The full timestamp belongs in a `title` or a
 * `dateTime` attribute beside it.
 */
export function formatTimeOfDay(value: string): string {
  const parsed = new Date(value);
  if (Number.isNaN(parsed.getTime())) {
    return value;
  }
  return parsed.toLocaleTimeString(undefined, { timeStyle: "medium" });
}
