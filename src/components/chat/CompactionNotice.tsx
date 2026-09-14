/**
 * The fold marker in the transcript (PLAN 7.3, Phase 14).
 *
 * Drawn right after the last folded message; the folded state is shown on
 * demand. It must make clear that nothing was deleted, only what the model
 * reads changed.
 */

import { useState } from "react";

import type { Compaction } from "../../ipc/bindings";
import { formatTimestamp } from "../../lib/format";

export default function CompactionNotice({
  compaction,
}: {
  readonly compaction: Compaction;
}) {
  const [open, setOpen] = useState(false);

  return (
    <section className="fold" aria-label="Folded conversation">
      <div className="fold__line">
        <span className="fold__label">
          {compaction.folded}{" "}
          {compaction.folded === 1 ? "message" : "messages"} above this point
          were folded into state{" "}
          <time dateTime={compaction.at}>
            {formatTimestamp(compaction.at)}
          </time>
        </span>
        <button
          type="button"
          className="link"
          aria-expanded={open}
          onClick={() => setOpen(!open)}
        >
          {open ? "Hide what it kept" : "What it kept"}
        </button>
      </div>

      <p className="fold__note">
        They are still here and still on disk — this only changes what the model
        carries into its next reply. Everything below this line still reaches it
        in full.
      </p>

      {open ? <pre className="fold__state">{compaction.state}</pre> : null}
    </section>
  );
}
