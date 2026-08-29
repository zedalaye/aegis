/**
 * The transcript.
 *
 * Two things beyond drawing bubbles.
 *
 * The streaming buffer is rendered as one extra assistant bubble at the end,
 * synthesized rather than stored. When `turn:message` arrives the real message
 * takes its place and the buffer empties, so the text never appears twice —
 * the swap happens in one render because both come from the same store update.
 *
 * Scrolling follows the stream, but only when the reader is already at the
 * bottom. Someone who has scrolled up to re-read something is reading it; a
 * pane that yanks itself back down every 50 ms while a reply streams is a pane
 * that cannot be read at all.
 */

import { useEffect, useLayoutEffect, useRef } from "react";

import type { Message } from "../../ipc/bindings";
import { isVisible, useSessions } from "../../state/sessions";
import { useSettings } from "../../state/settings";

import MessageBubble from "./MessageBubble";

/** How close to the bottom still counts as "at the bottom", in pixels. */
const STICK_THRESHOLD = 64;

/**
 * What an empty transcript says, which depends on what would answer it.
 *
 * The scripted provider understands three cues and a real model does not, so
 * naming them unconditionally would be advice that stops working the moment a
 * key is configured. The test mirrors `ProviderSettings::is_configured`, as
 * the badge in the header does.
 *
 * Settings not being loaded yet is its own case rather than a fall-through to
 * "no provider": that load resolves a frame or two after the window opens, and
 * telling a configured user their provider is missing — even briefly — is the
 * one wrong thing this paragraph could say.
 */
function EmptyTranscript() {
  const settings = useSettings((s) => s.settings);
  const configured =
    settings !== null &&
    settings.base_url.length > 0 &&
    settings.model.length > 0;

  const gate = (
    <>
      Anything it wants to do on this machine is asked about before it happens,
      and every tool call is written to the audit log whether you allow it or
      not.
    </>
  );

  if (settings === null) {
    return <p className="messages__empty">Nothing here yet. {gate}</p>;
  }

  if (configured) {
    return (
      <p className="messages__empty">
        Nothing here yet. Ask for something — the reply comes from{" "}
        <code>{settings.model}</code>. {gate}
      </p>
    );
  }

  return (
    <p className="messages__empty">
      Nothing here yet. Ask for something — no provider is configured, so the
      reply comes from the scripted provider and tells you what the runtime
      actually sent. Include <code>/write</code> in a message to make it ask
      for permission to write a file, <code>/run</code> to make it ask to run a
      command in your workspace, or <code>/capture</code> to make it ask for a
      picture of your screen. Any of them is how you see the approval gate
      work.
    </p>
  );
}

/** The bubble a streaming reply is drawn into before it is finalized. */
function streamingMessage(text: string): Message {
  return {
    id: "streaming",
    role: "assistant",
    text,
    tool_calls: [],
    tool_call_id: null,
    created_at: new Date().toISOString(),
  };
}

export default function MessageList() {
  const detail = useSessions((s) => s.detail);
  const streaming = useSessions((s) => s.streaming);

  const paneRef = useRef<HTMLDivElement | null>(null);
  const stuckRef = useRef(true);

  const messages = detail?.messages.filter(isVisible) ?? [];
  const buffer = streaming?.text ?? "";

  // Measured before the browser paints, so the decision is about where the
  // reader was, not where the new content has already pushed them.
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
  }, [messages.length, buffer, detail?.session.id]);

  if (detail === null) {
    return null;
  }

  const empty = messages.length === 0 && buffer === "";

  return (
    <div className="messages" ref={paneRef} aria-live="polite">
      {empty ? <EmptyTranscript /> : null}

      {messages.map((message) => (
        <MessageBubble key={message.id} message={message} />
      ))}

      {buffer === "" ? null : (
        <MessageBubble message={streamingMessage(buffer)} pending />
      )}

      {streaming !== null && buffer === "" ? (
        <p className="messages__thinking" role="status">
          Working…
        </p>
      ) : null}
    </div>
  );
}
