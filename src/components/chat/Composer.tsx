/**
 * The message box.
 *
 * The text is local state rather than store state: it changes on every
 * keystroke, and putting it in the store would re-render the transcript with
 * each one. It is cleared only once the send is accepted, so a rejected send —
 * `E_TURN_BUSY`, or a session deleted underneath — leaves what the user typed
 * exactly where they left it.
 *
 * Enter sends, Shift+Enter breaks the line. That is the convention every chat
 * client uses, and the box grows with the text so a multi-line message is
 * visible while it is being written.
 */

import { useEffect, useRef, useState } from "react";
import type { FormEvent, KeyboardEvent } from "react";

import { useApprovals } from "../../state/approvals";
import { useSessions } from "../../state/sessions";

/** Tallest the box grows before it scrolls instead, in pixels. */
const MAX_HEIGHT = 220;

export default function Composer() {
  const detail = useSessions((s) => s.detail);
  const streaming = useSessions((s) => s.streaming);
  const busy = useSessions((s) => s.busy);
  const send = useSessions((s) => s.send);
  const cancel = useSessions((s) => s.cancel);
  const waiting = useApprovals((s) => s.pending.length > 0);

  const [text, setText] = useState("");
  const boxRef = useRef<HTMLTextAreaElement | null>(null);

  const running = streaming !== null;
  const canSend = detail !== null && text.trim() !== "" && !running && !busy;

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

  const submit = (event?: FormEvent) => {
    event?.preventDefault();
    if (!canSend) {
      return;
    }
    const sent = text;
    setText("");
    void send(sent).catch(() => {
      // `send` holds its own failures in the store; restoring the text is the
      // only thing left worth doing here.
      setText((current) => (current === "" ? sent : current));
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
    <form className="composer" onSubmit={submit}>
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
    </form>
  );
}
