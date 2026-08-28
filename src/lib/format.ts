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
