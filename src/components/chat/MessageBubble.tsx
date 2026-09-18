/**
 * One message in the transcript.
 *
 * User and assistant text is markdown drawn through the explorer's parser,
 * which is the sanitizer (PLAN 7.20): a typed tree of elements and text nodes,
 * raw HTML left as text, no `<a href>`. Each streamed frame is parsed again.
 * Workspace links open the preview; web links open the OS browser through
 * `open_url`; a remote image is a link, never fetched.
 */

import { useCallback, useMemo, useState } from "react";
import type { ReactNode } from "react";

import type { Message } from "../../ipc/bindings";
import { openUrl } from "../../ipc/commands";
import { toIpcError } from "../../lib/errors";
import { formatTimestamp } from "../../lib/format";
import { useApprovals } from "../../state/approvals";
import { useExplorer } from "../../state/explorer";
import { useProjects } from "../../state/projects";
import { useSessions } from "../../state/sessions";
import { AssetImage, WorkspaceImage } from "../markdown/Images";
import Markdown from "../markdown/Markdown";
import type { EmbeddedImage } from "../markdown/Markdown";

import ToolCallCard from "./ToolCallCard";

/** A path as compared against the session's captures. */
function pathKey(path: string): string {
  return path.replace(/\\/g, "/").toLowerCase();
}

/** The session's capture paths, keyed for lookup, from the open transcript. */
function useCaptures(): ReadonlyMap<string, string> {
  const messages = useSessions((s) => s.detail?.messages);
  return useMemo(() => {
    const captures = new Map<string, string>();
    for (const message of messages ?? []) {
      for (const call of message.tool_calls) {
        if (call.image_path !== null) {
          captures.set(pathKey(call.image_path), call.image_path);
        }
      }
    }
    return captures;
  }, [messages]);
}

/** A message's text as markdown, with its links and images wired up. */
function MessageText({
  text,
  trailer,
}: {
  readonly text: string;
  readonly trailer: ReactNode;
}) {
  const projectId = useProjects((s) => s.detail?.project.id ?? null);
  const openPanel = useExplorer((s) => s.openPanel);
  const openPath = useExplorer((s) => s.openPath);
  const captures = useCaptures();
  const [linkError, setLinkError] = useState<string | null>(null);

  const onOpenPath = useCallback(
    (candidates: readonly string[]) => {
      if (projectId === null) {
        return;
      }
      void openPanel(projectId).then(() => openPath(candidates));
    },
    [projectId, openPanel, openPath],
  );

  const onOpenUrl = useCallback((url: string) => {
    setLinkError(null);
    openUrl(url).catch((cause: unknown) => {
      setLinkError(toIpcError(cause, "open_url").message);
    });
  }, []);

  const drawImage = useCallback(
    (image: EmbeddedImage): ReactNode => {
      const label = image.alt.length > 0 ? `[image: ${image.alt}]` : "[image]";
      const fallback = <span className="md__image" title={image.src}>{label}</span>;
      // Only a capture this session made; any other absolute path stays text.
      const capture = captures.get(pathKey(image.src));
      if (capture !== undefined) {
        return <AssetImage path={capture} alt={image.alt} fallback={fallback} />;
      }
      if (image.target !== null && projectId !== null) {
        return (
          <WorkspaceImage
            projectId={projectId}
            path={image.target}
            alt={image.alt}
            fallback={fallback}
          />
        );
      }
      return null;
    },
    [captures, projectId],
  );

  return (
    <>
      <Markdown
        className="bubble__md"
        source={text}
        base=""
        onOpenPath={onOpenPath}
        onOpenUrl={onOpenUrl}
        drawImage={drawImage}
        trailer={trailer}
      />
      {linkError === null ? null : (
        <p className="bubble__note" role="status">
          {linkError}
        </p>
      )}
    </>
  );
}

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
  const caret = pending ? (
    <span className="bubble__caret" aria-hidden="true" />
  ) : null;

  // Which of this message's calls, if any, the open approval is about. The
  // card and the dialog then name the same call, so a user reading the prompt
  // can see which line of the transcript it belongs to.
  //
  // The array itself is selected rather than a derived list: a selector that
  // built a new array on every read would never compare equal, and the bubble
  // would re-render on every store change in the application.
  const waiting = useApprovals((s) => s.pending);

  // Selected as the whole map for the same reason: a selector that built a
  // per-message object would never compare equal, and this bubble would
  // re-render on every frame of a command running in another one.
  const output = useSessions((s) => s.output);

  return (
    <article className={`bubble bubble--${message.role}`}>
      <header className="bubble__meta">
        <span className="bubble__speaker">{SPEAKER[message.role]}</span>
        <time className="bubble__time" dateTime={message.created_at}>
          {formatTimestamp(message.created_at)}
        </time>
      </header>

      {message.text === "" ? null : message.role === "user" ||
        message.role === "assistant" ? (
        <MessageText text={message.text} trailer={caret} />
      ) : (
        <p className="bubble__text">
          {message.text}
          {caret}
        </p>
      )}

      {hasCalls ? (
        <ul className="bubble__tools" aria-label="Tool calls">
          {message.tool_calls.map((call) => (
            <ToolCallCard
              key={call.call_id}
              call={call}
              output={output[call.call_id] ?? null}
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
