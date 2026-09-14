/**
 * The transcript.
 *
 * - The streaming buffer is one synthesized bubble, replaced by the real
 *   message in the same store update.
 * - Scrolling follows only if the reader was already at the bottom, tracked
 *   from their own scrolling (see the effects).
 * - A compaction draws a fold marker; everything above it is still shown
 *   (Phase 14).
 */

import { Fragment, useCallback, useLayoutEffect, useRef } from "react";

import type { Message } from "../../ipc/bindings";
import { isVisible, useSessions } from "../../state/sessions";
import { isConfigured, useSettings } from "../../state/settings";

import CompactionNotice from "./CompactionNotice";
import MessageBubble from "./MessageBubble";

/** How close to the bottom still counts as "at the bottom", in pixels. */
const STICK_THRESHOLD = 64;

/**
 * The empty-transcript hint: the scripted provider's cues only when no provider
 * is configured (mirrors `ProviderSettings::is_configured`), and nothing while
 * settings are still loading.
 */
function EmptyTranscript() {
  const settings = useSettings((s) => s.settings);
  const configured = settings !== null && isConfigured(settings);

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
      command in your workspace, <code>/capture</code> to make it ask for a
      picture of your screen, or <code>/remember</code> to make it ask to
      remember something. Any of them is how you see the approval gate work.
    </p>
  );
}

/**
 * The last visible message at or before the fold pointer (often an undrawn
 * `tool` message); `null` puts the marker at the top.
 */
function foldAfter(
  messages: readonly Message[],
  throughId: string,
): string | null {
  const at = messages.findIndex((message) => message.id === throughId);
  if (at < 0) {
    return null;
  }

  for (let index = at; index >= 0; index -= 1) {
    const message = messages[index];
    if (message !== undefined && isVisible(message)) {
      return message.id;
    }
  }
  return null;
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
  const compaction = detail?.compaction ?? null;
  const foldAnchor =
    compaction === null || detail === null
      ? null
      : foldAfter(detail.messages, compaction.through_message_id);

  // Set only from scroll events: measured after a render, the new bubble's
  // height would read as "scrolled up". Our own scroll-to-bottom sets `true`.
  const measure = useCallback(() => {
    const pane = paneRef.current;
    if (pane === null) {
      return;
    }
    const distance = pane.scrollHeight - pane.scrollTop - pane.clientHeight;
    stuckRef.current = distance <= STICK_THRESHOLD;
  }, []);

  // A callback ref rather than a mount effect, because the pane is not in the
  // DOM on the first render — no session is open yet — and an effect keyed on
  // `[]` would bind to nothing and never try again, leaving the listener
  // unattached for the life of the window.
  const attachPane = useCallback(
    (node: HTMLDivElement | null) => {
      paneRef.current?.removeEventListener("scroll", measure);
      paneRef.current = node;
      node?.addEventListener("scroll", measure, { passive: true });
    },
    [measure],
  );

  // A new session starts following. A layout effect declared before the
  // scrolling one, since layout effects run in declaration order.
  useLayoutEffect(() => {
    stuckRef.current = true;
  }, [detail?.session.id]);

  // Every render: bubbles also grow in place without `messages.length`
  // changing.
  useLayoutEffect(() => {
    const pane = paneRef.current;
    if (pane !== null && stuckRef.current) {
      pane.scrollTop = pane.scrollHeight;
    }
  });

  if (detail === null) {
    return null;
  }

  const empty = messages.length === 0 && buffer === "";

  return (
    <div className="messages" ref={attachPane} aria-live="polite">
      {empty ? <EmptyTranscript /> : null}

      {/* Nothing drawn is above the fold — every folded message was a tool
          result — so the marker opens the pane. */}
      {compaction !== null && foldAnchor === null ? (
        <CompactionNotice compaction={compaction} />
      ) : null}

      {messages.map((message) => (
        <Fragment key={message.id}>
          <MessageBubble message={message} />
          {compaction !== null && foldAnchor === message.id ? (
            <CompactionNotice compaction={compaction} />
          ) : null}
        </Fragment>
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
