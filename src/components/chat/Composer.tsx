/**
 * The message box.
 *
 * Local state (no transcript re-render per keystroke), cleared only once a
 * send is accepted. Enter sends, Shift+Enter breaks; the box auto-grows.
 *
 * Images (PLAN 7.20) come from the picker, which runs in Rust, or from a drop
 * onto the box. Either way the runtime copies them under the app's data and
 * hands back ids; the window shows the copy through `asset:` and sends only
 * the ids. Attach is hidden only when the model's catalog entry says it takes
 * no images — unknown still offers it, and a refusal fails the turn visibly.
 */

import { useEffect, useRef, useState } from "react";
import type { FormEvent, KeyboardEvent } from "react";

import type { AttachReport, Attached } from "../../ipc/bindings";
import { attachmentDrop, attachmentPick } from "../../ipc/commands";
import { on } from "../../ipc/events";
import { elementAtDrop } from "../../lib/drop";
import { toIpcError } from "../../lib/errors";
import { formatBytes } from "../../lib/format";
import { useApprovals } from "../../state/approvals";
import { useBinding } from "../../state/binding";
import { useSessions } from "../../state/sessions";
import { useSettings } from "../../state/settings";
import { AssetImage } from "../markdown/Images";

/** Tallest the box grows before it scrolls instead, in pixels. */
const MAX_HEIGHT = 220;

/** Most images one message carries; the runtime enforces the same number. */
const MAX_ATTACHED = 8;

/** What a report says about the files it did not take, or `null`. */
function refusals(report: AttachReport): string | null {
  if (report.refused.length === 0) {
    return null;
  }
  return report.refused
    .map((refused) => `${refused.name}: ${refused.reason}`)
    .join("; ");
}

export default function Composer() {
  const detail = useSessions((s) => s.detail);
  const streaming = useSessions((s) => s.streaming);
  const busy = useSessions((s) => s.busy);
  const send = useSessions((s) => s.send);
  const cancel = useSessions((s) => s.cancel);
  const waiting = useApprovals((s) => s.pending.length > 0);
  const binding = useBinding(detail?.session ?? null);
  const takesImages = useSettings((s) =>
    binding.row === undefined ? undefined : s.vision[binding.row.id]?.[binding.model],
  );

  const [text, setText] = useState("");
  const [attached, setAttached] = useState<readonly Attached[]>([]);
  const [notice, setNotice] = useState<string | null>(null);
  const [attaching, setAttaching] = useState(false);
  const boxRef = useRef<HTMLTextAreaElement | null>(null);
  const formRef = useRef<HTMLFormElement | null>(null);

  const running = streaming !== null;
  const canSend =
    detail !== null &&
    (text.trim() !== "" || attached.length > 0) &&
    !running &&
    !busy &&
    !attaching;
  const canAttach = detail !== null && takesImages !== false;

  // Reset to one row first, or the box can only ever grow: `scrollHeight` of
  // an already-tall element measures the height it was given, not the text.
  useEffect(() => {
    const box = boxRef.current;
    if (box === null) {
      return;
    }
    box.style.height = "auto";
    box.style.height = `${Math.min(box.scrollHeight, MAX_HEIGHT)}px`;
  }, [text]);

  // Another session's images are not this one's.
  useEffect(() => {
    setAttached([]);
    setNotice(null);
  }, [detail?.session.id]);

  const take = (report: AttachReport) => {
    setAttached((current) => [...current, ...report.attached].slice(0, MAX_ATTACHED));
    setNotice(refusals(report));
  };

  const run = (label: string, attach: () => Promise<AttachReport>) => {
    setAttaching(true);
    setNotice(null);
    attach()
      .then(take)
      .catch((cause: unknown) => setNotice(toIpcError(cause, label).message))
      .finally(() => setAttaching(false));
  };

  // A drop onto the box attaches; the project list and Files have their own
  // targets, so a drop lands in exactly one place.
  const canAttachRef = useRef(canAttach);
  canAttachRef.current = canAttach;
  useEffect(() => {
    const pending = on("workspace:dropped", (drop) => {
      const target = elementAtDrop(drop.x, drop.y);
      if (target === null || formRef.current === null || !formRef.current.contains(target)) {
        return;
      }
      if (!canAttachRef.current) {
        setNotice("This model does not take images.");
        return;
      }
      run("attachment_drop", () => attachmentDrop(drop.drop_id));
    });
    return () => {
      void pending.then((detach) => detach());
    };
  }, []);

  const submit = (event?: FormEvent) => {
    event?.preventDefault();
    if (!canSend) {
      return;
    }
    const sent = text;
    const images = attached;
    setText("");
    setAttached([]);
    setNotice(null);
    void send(sent, images).catch(() => {
      // `send` holds its own failures in the store; restoring the draft is
      // the only thing left worth doing here.
      setText((current) => (current === "" ? sent : current));
      setAttached((current) => (current.length === 0 ? images : current));
    });
  };

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    // `isComposing` is an IME mid-word: Enter is committing a candidate there,
    // not sending a message.
    if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) {
      event.preventDefault();
      submit();
    }
  };

  return (
    <form className="composer" onSubmit={submit} ref={formRef}>
      {attached.length === 0 ? null : (
        <ul className="composer__attached" aria-label="Attached images">
          {attached.map((item) => (
            <li key={item.id} className="composer__chip">
              <AssetImage
                className="composer__thumb"
                path={item.attachment.path}
                alt=""
                fallback={<span className="composer__thumb" />}
              />
              <span className="composer__name" title={item.name}>
                {item.name}
              </span>
              <span className="composer__size">{formatBytes(item.bytes)}</span>
              <button
                type="button"
                className="composer__remove"
                aria-label={`Remove ${item.name}`}
                onClick={() =>
                  setAttached((current) => current.filter((held) => held.id !== item.id))
                }
              >
                ×
              </button>
            </li>
          ))}
        </ul>
      )}

      <textarea
        ref={boxRef}
        className="composer__box"
        rows={1}
        value={text}
        placeholder={
          detail === null ? "Open a session to start" : "Send a message…"
        }
        aria-label="Message"
        disabled={detail === null}
        onChange={(event) => setText(event.target.value)}
        onKeyDown={onKeyDown}
      />

      {notice === null ? null : (
        <p className="composer__notice" role="status">
          {notice}
        </p>
      )}

      <div className="composer__actions">
        <span className="composer__hint">
          {/*
            Waiting for an answer is not streaming, and saying so would tell
            the user to wait for something that is waiting for them. Stop stays
            available either way: cancelling a turn parked on an approval is a
            supported way out of it.
          */}
          {waiting
            ? "Waiting for your answer above"
            : running
              ? "Streaming…"
              : "Enter to send, Shift+Enter for a new line"}
        </span>
        <div className="composer__buttons">
          {canAttach ? (
            <button
              type="button"
              className="button"
              title="Attach images: PNG, JPEG, GIF or WebP, up to 16 MB each. You can also drop them here."
              disabled={attaching || running || attached.length >= MAX_ATTACHED}
              onClick={() => run("attachment_pick", attachmentPick)}
            >
              {attaching ? "Attaching…" : "Attach"}
            </button>
          ) : null}
          {running ? (
            <button
              type="button"
              className="button"
              onClick={() => void cancel()}
              // Cancelling is the one control that must stay live while a turn
              // runs, so it is deliberately not disabled by `busy`.
            >
              Stop
            </button>
          ) : (
            <button type="submit" className="button button--primary" disabled={!canSend}>
              Send
            </button>
          )}
        </div>
      </div>
    </form>
  );
}
