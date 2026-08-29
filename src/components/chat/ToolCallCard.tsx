/**
 * One tool call in the transcript.
 *
 * The transcript is the record of what the agent did, so a call appears here
 * whatever became of it — auto-allowed, approved, refused, failed, or abandoned
 * by a cancel. A reply that quietly touched the filesystem and left no trace
 * would be the worst possible default.
 *
 * Two rules about what is shown.
 *
 * **The arguments are the ones the model sent**, not the ones policy resolved,
 * because that is what a user is checking when they go back and read a call
 * they allowed. They are rendered as plain text inside a `<pre>`: model output
 * is untrusted input, and the WebView holds the whole UI.
 *
 * **The summary is one line.** A tool's real output can be 256 KB; the model
 * gets the envelope, the transcript gets a sentence. Arguments are collapsed
 * behind a disclosure for the same reason — the card has to stay skimmable in
 * a conversation that made twenty calls.
 */

import { useState } from "react";

import type { ToolCallRecord, ToolCallStatus } from "../../ipc/bindings";

/** What each status is called, and how it should read. */
const STATUS: Record<ToolCallStatus, string> = {
  pending: "waiting for policy",
  approved: "approved",
  denied: "refused",
  running: "running",
  ok: "done",
  error: "failed",
  cancelled: "cancelled",
};

/** Pretty-prints the arguments, falling back to the raw string. */
function formatArgs(argsJson: string): string {
  try {
    return JSON.stringify(JSON.parse(argsJson) as unknown, null, 2);
  } catch {
    // The model sent something that never parsed. Showing it verbatim is more
    // honest than showing nothing: it is exactly what the runtime refused.
    return argsJson;
  }
}

export default function ToolCallCard({
  call,
  awaiting = false,
}: {
  readonly call: ToolCallRecord;
  /** True while this call is the one an open approval is about. */
  readonly awaiting?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const status = awaiting ? "waiting for you" : STATUS[call.status];

  return (
    <li
      className={`toolcall toolcall--${call.status}${
        awaiting ? " toolcall--awaiting" : ""
      }`}
    >
      <div className="toolcall__head">
        <span className="toolcall__tool">{call.tool}</span>
        <span className="toolcall__status">{status}</span>
        <button
          type="button"
          className="toolcall__toggle"
          aria-expanded={open}
          onClick={() => setOpen((shown) => !shown)}
        >
          {open ? "Hide arguments" : "Arguments"}
        </button>
      </div>

      {call.summary === null ? null : (
        <p className="toolcall__summary">{call.summary}</p>
      )}

      {open ? (
        <pre className="toolcall__args">{formatArgs(call.args_json)}</pre>
      ) : null}
    </li>
  );
}
