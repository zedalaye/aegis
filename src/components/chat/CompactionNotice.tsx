/**
 * The fold marker in the transcript (PLAN 7.3, Phase 14).
 *
 * Drawn in place, immediately after the last message that folded, because that
 * is where the fold actually is. A banner at the top of the pane would say the
 * same words and answer a different question: what a reader wants to know is
 * *from here on, the model is reading this again*, and *above this line, it is
 * reading four lines of state instead*.
 *
 * The state it folded to is shown on demand rather than by default. It is the
 * answer to "what does it still know", which is worth being able to check and
 * is not worth six lines of the transcript on every scroll past.
 *
 * The thing this component has to make unambiguous is that nothing was
 * deleted. The messages above the line are still on screen, still on disk, and
 * still in the audit log — a fold changes what the *model* carries, and a
 * reader who thought their conversation had been trimmed would be right to be
 * alarmed and wrong about what happened.
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
