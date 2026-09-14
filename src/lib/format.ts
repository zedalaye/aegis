/**
 * Presentation helpers.
 *
 * Formatting only — nothing here reads or decides anything. Paths arrive from
 * Rust already canonical, so these functions never normalize, only display.
 */

import type { Cost } from "../ipc/bindings";

/** Matches a path separator on either platform family. */
const SEPARATOR = /[\\/]+/;

/**
 * The last segment of a path, ignoring trailing separators; a root names
 * itself.
 */
export function folderName(path: string): string {
  const segments = path.split(SEPARATOR).filter((segment) => segment.length > 0);
  return segments.at(-1) ?? path;
}

/** Shortens a long path by eliding its middle, keeping both ends. */
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
 * An RFC3339 timestamp in local time; unparseable values are shown verbatim.
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

/** A byte count in powers of 1024, one decimal above bytes ("1.4 KB"). */
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

/** A duration in ms, s or min; sub-second values keep their milliseconds. */
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
 * An RFC3339 timestamp as local clock time with seconds, for dense lists;
 * put the full timestamp in a `title` or `dateTime`.
 */
export function formatTimeOfDay(value: string): string {
  const parsed = new Date(value);
  if (Number.isNaN(parsed.getTime())) {
    return value;
  }
  return parsed.toLocaleTimeString(undefined, { timeStyle: "medium" });
}

/** A token count: exact below 1k, then `k` and `M`. */
export function formatTokens(tokens: number): string {
  if (!Number.isFinite(tokens) || tokens < 0) {
    return "—";
  }
  if (tokens < 1000) {
    return String(Math.round(tokens));
  }
  if (tokens < 1_000_000) {
    return `${(tokens / 1000).toFixed(1)}k`;
  }
  return `${(tokens / 1_000_000).toFixed(2)}M`;
}

/**
 * A {@link Cost} in one phrase: "at least" when some usage went unreported, a
 * dash when nothing was measured.
 */
export function formatCost(cost: Cost): string {
  if (cost.turns === 0) {
    return "—";
  }

  const total = formatTokens(cost.prompt_tokens + cost.completion_tokens);
  const turns = `${cost.turns} turn${cost.turns === 1 ? "" : "s"}`;
  return cost.unreported > 0
    ? `at least ${total} tokens · ${turns}`
    : `${total} tokens · ${turns}`;
}

/**
 * The whole-percent share of a {@link Cost}'s prompt served from cache — low
 * on a warm session means the prompt prefix keeps changing. `null` when no
 * prompt usage was reported (distinct from zero).
 */
export function cacheShare(cost: Cost): number | null {
  if (cost.prompt_tokens === 0) {
    return null;
  }
  return Math.round((cost.cache_read_tokens / cost.prompt_tokens) * 100);
}
