/**
 * The open project's folder, as a tree (PLAN 7.15).
 *
 * One folder at a time: a row's children are listed by the runtime when it is
 * opened, never before. Rows are buttons — a folder opens, a file previews —
 * and nothing else: no rename, no delete, no new file. A tree that could
 * change the folder would be a second write path around the approval gate.
 *
 * The cabinet is marked as the working surface without being made the only
 * thing here. `.aegis/` and `world/` are ordinary rows with a word beside
 * them, among the project's other files — a brief names inputs anywhere in the
 * workspace, and a tree that showed only the convention could not show those.
 *
 * Each row also says what a drop onto it would do, in a `data-drop` attribute
 * the panel reads when the OS reports where a drop landed. `.aegis/briefs/`
 * takes one; artefacts, the rest of the cabinet and the world refuse; every
 * other row is the project, and a drop there lands in briefs too.
 */

import type { TreeEntry, Zone } from "../../ipc/bindings";
import { formatBytes } from "../../lib/format";
import { useExplorer } from "../../state/explorer";

/** What a drop onto a row of each zone does, as the panel reads it. */
export const DROP_BY_ZONE: Record<Zone, string | undefined> = {
  plain: undefined,
  briefs: "brief",
  artefacts:
    "refuse:.aegis/artefacts/ is work coming out. A dropped file is a brief, so drop it on the project or on .aegis/briefs/.",
  cabinet:
    "refuse:Only .aegis/briefs/ takes a drop. Status, decisions and runbooks are written through a session, under the approval dialog.",
  world:
    "refuse:world/ is the constitution. A dropped file is a brief, not a source of the world.",
};

/** The one word beside a row that is part of the convention. */
function tag(entry: TreeEntry): string | null {
  switch (entry.path) {
    case ".aegis":
      return "shared files";
    case ".aegis/briefs":
      return "drop here";
    case "world":
      return "constitution";
    default:
      return null;
  }
}

function Row({ entry, depth }: { readonly entry: TreeEntry; readonly depth: number }) {
  const expanded = useExplorer((s) => s.expanded.includes(entry.path));
  const selected = useExplorer((s) => s.selected === entry.path);
  const toggleDir = useExplorer((s) => s.toggleDir);
  const select = useExplorer((s) => s.select);

  const isDir = entry.kind === "dir";
  const openable = !entry.outside && entry.kind !== "other";
  const label = tag(entry);

  const title = entry.outside
    ? `${entry.path} — a link that leads outside this workspace, so it is not opened here`
    : entry.kind === "other"
      ? `${entry.path} — not a file or a folder`
      : entry.ignored
        ? `${entry.path} — ignored by this repository`
        : entry.path;

  return (
    <li
      className="tree__item"
      role="treeitem"
      aria-expanded={isDir && openable ? expanded : undefined}
      aria-selected={selected}
      data-drop={DROP_BY_ZONE[entry.zone]}
      data-path={entry.path}
    >
      <button
        type="button"
        className={[
          "tree__row",
          isDir ? "tree__row--dir" : "",
          entry.ignored ? "tree__row--ignored" : "",
          selected ? "tree__row--selected" : "",
          entry.zone === "plain" ? "" : `tree__row--${entry.zone}`,
        ]
          .filter(Boolean)
          .join(" ")}
        style={{ paddingLeft: `${0.4 + depth * 0.9}rem` }}
        title={title}
        disabled={!openable}
        onClick={() => void (isDir ? toggleDir(entry.path) : select(entry.path))}
      >
        <span aria-hidden="true" className="tree__caret">
          {isDir ? (expanded ? "▾" : "▸") : ""}
        </span>
        <span className="tree__name">{entry.name}</span>
        {label === null ? null : <span className="tree__tag">{label}</span>}
        {entry.outside ? <span className="tree__tag">outside</span> : null}
        {entry.bytes === null ? null : (
          <span className="tree__size">{formatBytes(entry.bytes)}</span>
        )}
      </button>

      {isDir && openable && expanded ? (
        <ul className="tree__group" role="group">
          <Folder dir={entry.path} depth={depth + 1} />
        </ul>
      ) : null}
    </li>
  );
}

/** The rows of one folder, and what its listing left out. */
function Folder({ dir, depth }: { readonly dir: string; readonly depth: number }) {
  const listing = useExplorer((s) => s.listings[dir]);
  const pad = { paddingLeft: `${0.4 + depth * 0.9 + 1.1}rem` };

  if (listing === undefined) {
    return (
      <li className="tree__note" style={pad}>
        Reading…
      </li>
    );
  }

  return (
    <>
      {listing.entries.length === 0 && listing.hidden === 0 ? (
        <li className="tree__note" style={pad}>
          Empty
        </li>
      ) : null}
      {listing.entries.map((entry) => (
        <Row key={entry.path} entry={entry} depth={depth} />
      ))}
      {listing.more > 0 ? (
        <li className="tree__note" style={pad}>
          …and {listing.more} more not listed
        </li>
      ) : null}
      {listing.hidden > 0 ? (
        <li className="tree__note" style={pad}>
          {listing.hidden} ignored {listing.hidden === 1 ? "entry" : "entries"} hidden
        </li>
      ) : null}
    </>
  );
}

export default function FileTree() {
  return (
    <ul className="tree" role="tree" aria-label="Files in this workspace">
      <Folder dir="" depth={0} />
    </ul>
  );
}
