/**
 * One message in the transcript.
 *
 * Text is rendered as plain text, deliberately. A model's output is untrusted
 * input, and the WebView holds the whole UI — rendering it as markup would put
 * whatever the model said one escaping bug away from the DOM. `white-space:
 * pre-wrap` keeps the paragraphs and line breaks a reader needs; a markdown
 * renderer is a Phase 10 question, and it will need a sanitizer with it.
 */

import type { Message } from "../../ipc/bindings";
import { formatTimestamp } from "../../lib/format";
import { useApprovals } from "../../state/approvals";

import ToolCallCard from "./ToolCallCard";

/** How a role is named to the reader. */
const SPEAKER: Record<Message["role"], string> = {
  user: "You",
  assistant: "Aegis",
  tool: "Tool",
  system: "System",
};

export default function MessageBubble({
  message,
  pending = false,
}: {
  readonly message: Message;
  /** True while this message is still streaming, for the caret. */
  readonly pending?: boolean;
}) {
  const hasCalls = message.tool_calls.length > 0;

  // Which of this message's calls, if any, the open approval is about. The
  // card and the dialog then name the same call, so a user reading the prompt
  // can see which line of the transcript it belongs to.
  //
  // The array itself is selected rather than a derived list: a selector that
  // built a new array on every read would never compare equal, and the bubble
  // would re-render on every store change in the application.
  const waiting = useApprovals((s) => s.pending);

  return (
    <article className={`bubble bubble--${message.role}`}>
      <header className="bubble__meta">
        <span className="bubble__speaker">{SPEAKER[message.role]}</span>
        <time className="bubble__time" dateTime={message.created_at}>
          {formatTimestamp(message.created_at)}
        </time>
      </header>

      {message.text === "" ? null : (
        <p className="bubble__text">
          {message.text}
          {pending ? <span className="bubble__caret" aria-hidden="true" /> : null}
        </p>
      )}

      {hasCalls ? (
        <ul className="bubble__tools" aria-label="Tool calls">
          {message.tool_calls.map((call) => (
            <ToolCallCard
              key={call.call_id}
              call={call}
              awaiting={waiting.some(
                (request) => request.call_id === call.call_id,
              )}
            />
          ))}
        </ul>
      ) : null}
    </article>
  );
}
