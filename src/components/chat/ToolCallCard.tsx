/**
 * One tool call in the transcript.
 *
 * The transcript is the record of what the agent did, so a call appears here
 * whatever became of it — auto-allowed, approved, refused, failed, or abandoned
 * by a cancel. A reply that quietly touched the filesystem and left no trace
 * would be the worst possible default.
 *
 * Three rules about what is shown.
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
 *
 * The text arrives already cleaned: the runtime strips ANSI escape sequences,
 * so this pane never has to be a terminal emulator, and what it shows is what
 * the model was given.
 *
 * **A running command's output is shown as it arrives.** That is the one
 * exception to the rule above, and it is the point of `shell_exec`: a command
 * that takes two minutes and shows nothing until it is done is
 * indistinguishable from a hang. The pane is bounded, it follows the output
 * the way the transcript follows a reply, and it says when it is not showing
 * everything. It is a live view, not a record — nothing is on disk but the
 * summary, so it is gone when the session is re-opened.
 *
 * A capture is the one result that is a picture, and it is shown as one. The
 * bytes never come through the IPC channel (PLAN 5.4): the record carries the
 * PNG's path, `convertFileSrc` turns it into an `asset:` URL, and the runtime
 * has scoped that protocol to the capture directory and nothing else — a path
 * outside it comes back 403 rather than being read. Unlike the output pane
 * this *is* a record: the path is persisted with the call, so re-opening the
 * session shows the capture again. It is also the one place the transcript
 * shows a person something the model was not shown; the model got a path, a
 * size and a digest.
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
 * What a command has printed, following the output as it arrives.
 *
 * Scrolling sticks to the bottom only while the reader is already there, for
 * the same reason the transcript does: someone who has scrolled up to read a
 * compiler error is reading it, and a pane that yanks itself back down every
 * 50 ms cannot be read at all.
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
 * The capture a call produced, at a size that fits in a transcript.
 *
 * Clicking it grows it in place rather than opening it anywhere. A link would
 * be a navigation, and this window is the application — nothing in the UI
 * should be one click away from leaving it. Growing it costs nothing extra:
 * the `<img>` already holds the full-resolution file, and only the box around
 * it changes.
 *
 * A capture that has since been deleted from disk simply fails to load, and
 * says so. The summary line above it still records what was taken and where,
 * which is the part that was never in the picture.
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
