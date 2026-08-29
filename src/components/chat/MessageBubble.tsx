/**
 * One message in the transcript.
 *
 * Text is rendered as plain text, deliberately. A model's output is untrusted
 * input, and the WebView holds the whole UI — rendering it as markup would put
 * whatever the model said one escaping bug away from the DOM. `white-space:
 * pre-wrap` keeps the paragraphs and line breaks a reader needs; a markdown
 * renderer is a Phase 10 question, and it will need a sanitizer with it.
 */

import type { Message, ToolCallRecord } from "../../ipc/bindings";
import { formatTimestamp } from "../../lib/format";

/** How a role is named to the reader. */
const SPEAKER: Record<Message["role"], string> = {
  user: "You",
  assistant: "Aegis",
  tool: "Tool",
  system: "System",
};

/**
 * One tool call, in the compact form the transcript shows.
 *
 * The full card — arguments, diff preview, the approval controls — arrives
 * with the approval gate in Phase 6. What matters here is that a call is
 * visible at all: a reply that quietly touched the filesystem and left no
 * trace in the transcript would be the worst possible default.
 */
function ToolCallLine({ call }: { readonly call: ToolCallRecord }) {
  return (
    <li className={`toolcall toolcall--${call.status}`}>
      <span className="toolcall__tool">{call.tool}</span>
      <span className="toolcall__status">{call.status}</span>
      {call.summary === null ? null : (
        <span className="toolcall__summary">{call.summary}</span>
      )}
    </li>
  );
}

export default function MessageBubble({
  message,
  pending = false,
}: {
  readonly message: Message;
  /** True while this message is still streaming, for the caret. */
  readonly pending?: boolean;
}) {
  const hasCalls = message.tool_calls.length > 0;

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
            <ToolCallLine key={call.call_id} call={call} />
          ))}
        </ul>
      ) : null}
    </article>
  );
}
