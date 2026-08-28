/**
 * The workspace path, shown the way it should be read.
 *
 * The workspace root is the boundary every later phase measures against —
 * reads inside it are automatic, everything outside always asks — so it is
 * worth a persistent, unambiguous surface rather than a line of body text.
 * The full path is always in the `title`, because an elided one must never be
 * the only copy the user can see.
 */

import { shortenPath } from "../../lib/format";

type WorkspaceBadgeProps = {
  readonly path: string;
  /**
   * Whether the folder is present right now. `false` renders a warning: the
   * project is still usable as a record, but nothing can be done inside it.
   */
  readonly exists: boolean;
  /** Characters before the middle of the path is elided. */
  readonly maxLength?: number;
};

export default function WorkspaceBadge({
  path,
  exists,
  maxLength,
}: WorkspaceBadgeProps) {
  const shown = shortenPath(path, maxLength);

  return (
    <span
      className={`workspace${exists ? "" : " workspace--missing"}`}
      title={exists ? path : `${path} — this folder is not there any more`}
    >
      <span aria-hidden="true" className="workspace__icon">
        {exists ? "▣" : "⚠"}
      </span>
      <span className="workspace__path">{shown}</span>
      {exists ? null : <span className="workspace__note">missing</span>}
    </span>
  );
}
