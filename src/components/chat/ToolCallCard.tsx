/**
 * One tool call in the transcript.
 *
 * Every call is shown, whatever became of it.
 *
 * - Arguments are the model's own, as plain text in a `<pre>` (untrusted),
 *   collapsed by default.
 * - The summary is one line; ANSI is already stripped by the runtime.
 * - A running command's output streams into a bounded live pane that is not
 *   persisted.
 * - A capture is shown from its persisted path via the capture-scoped `asset:`
 *   protocol (PLAN 5.4); the model only got path, size and digest.
 */

import { convertFileSrc } from "@tauri-apps/api/core";
import { useEffect, useLayoutEffect, useRef, useState } from "react";

import type { ToolCallRecord, ToolCallStatus } from "../../ipc/bindings";
import type { ToolOutput } from "../../state/sessions";

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

/** How close to the bottom still counts as "at the bottom", in pixels. */
const STICK_THRESHOLD = 24;

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

/**
 * What a command has printed, following the bottom only while the reader is
 * there.
 */
function OutputPane({ output }: { readonly output: ToolOutput }) {
  const paneRef = useRef<HTMLPreElement | null>(null);
  const stuckRef = useRef(true);

  useLayoutEffect(() => {
    const pane = paneRef.current;
    if (pane === null) {
      return;
    }
    const distance = pane.scrollHeight - pane.scrollTop - pane.clientHeight;
    stuckRef.current = distance <= STICK_THRESHOLD;
  });

  useEffect(() => {
    const pane = paneRef.current;
    if (pane !== null && stuckRef.current) {
      pane.scrollTop = pane.scrollHeight;
    }
  }, [output]);

  return (
    <pre className="toolcall__output" ref={paneRef} aria-label="Command output">
      {output.runs.map((run, index) => (
        <span
          // The runs are an append-only list whose entries only ever grow, so
          // the index is the identity here — nothing is inserted or reordered.
          key={index}
          className={
            run.stream === "stderr" ? "toolcall__stderr" : undefined
          }
        >
          {run.text}
        </span>
      ))}
      {output.truncated ? (
        <span className="toolcall__elision">
          {"\n… the command printed more than is kept here\n"}
        </span>
      ) : null}
    </pre>
  );
}

/**
 * The capture a call produced; clicking grows it in place (never navigates). A
 * deleted file shows a load failure.
 */
function CaptureThumbnail({ path }: { readonly path: string }) {
  const [broken, setBroken] = useState(false);
  const [large, setLarge] = useState(false);

  if (broken) {
    return (
      <p className="toolcall__missing">
        This capture is no longer at <code>{path}</code>.
      </p>
    );
  }

  return (
    <button
      type="button"
      className={`toolcall__capture${large ? " toolcall__capture--large" : ""}`}
      onClick={() => setLarge((shown) => !shown)}
      aria-expanded={large}
      title={path}
    >
      <img
        className="toolcall__thumb"
        // The scoped `asset:` URL. The bytes are read by the runtime from the
        // capture directory; nothing about the image passes through `invoke`.
        src={convertFileSrc(path)}
        alt={`Screen capture saved to ${path}`}
        onError={() => setBroken(true)}
      />
    </button>
  );
}

export default function ToolCallCard({
  call,
  output = null,
  awaiting = false,
}: {
  readonly call: ToolCallRecord;
  /** What this call has printed so far, when it printed anything. */
  readonly output?: ToolOutput | null;
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

      {call.image_path === null ? null : (
        <CaptureThumbnail path={call.image_path} />
      )}

      {output === null || output.runs.length === 0 ? null : (
        <OutputPane output={output} />
      )}
    </li>
  );
}
