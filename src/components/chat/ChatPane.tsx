/**
 * The work area when a project is open: a session header, the transcript, and
 * the composer.
 *
 * The header shows the turn's activity, identity and model. Without an open
 * session it distinguishes "no sessions yet" from "none chosen".
 */

import { useProjects } from "../../state/projects";
import { useApprovals } from "../../state/approvals";
import { useSessions } from "../../state/sessions";
import {
  cacheShare,
  formatBytes,
  formatTimestamp,
  formatTokens,
} from "../../lib/format";

import AgentBadge from "../agents/AgentBadge";
import ApprovalDialog from "../approvals/ApprovalDialog";
import GrantList from "../approvals/GrantList";
import Composer from "./Composer";
import MessageList from "./MessageList";
import ModelBadge from "./ModelBadge";

/** What the session header says on its right-hand side. */
function StatusLine() {
  const session = useSessions((s) => s.detail?.session ?? null);
  const streaming = useSessions((s) => s.streaming);
  const waiting = useApprovals((s) => s.pending.length > 0);

  if (session === null) {
    return null;
  }
  // Waiting for a person outranks streaming: it is the state the user can do
  // something about, and it is why nothing else is happening.
  if (waiting) {
    return (
      <span className="chat__status chat__status--awaiting">
        waiting for you
      </span>
    );
  }
  // While a tool call's arguments stream (a large `fs_write` takes minutes),
  // show their size. "sent", because it counts escaped JSON, not the file.
  if (streaming?.drafting != null) {
    const { tool, bytes } = streaming.drafting;
    return (
      <span className="chat__status chat__status--running">
        {tool === null ? "writing a tool call" : `writing ${tool}`} ·{" "}
        {formatBytes(bytes)} sent
      </span>
    );
  }
  if (streaming !== null) {
    return <span className="chat__status chat__status--running">streaming…</span>;
  }
  if (session.state === "error") {
    return <span className="chat__status chat__status--error">last turn failed</span>;
  }
  return (
    <time className="chat__status" dateTime={session.updated_at}>
      {formatTimestamp(session.updated_at)}
    </time>
  );
}

/**
 * What this conversation has spent (Phase 17). Hidden until a turn is charged;
 * "at least" when usage went unreported. The cached share is in the badge
 * because it drives the real cost; hidden when the provider does not cache.
 */
function CostBadge() {
  const cost = useSessions((s) => s.detail?.session.cost ?? null);
  if (cost === null || cost.turns === 0) {
    return null;
  }

  const total = cost.prompt_tokens + cost.completion_tokens;
  const cached = cacheShare(cost);
  const caching = cached !== null && (cost.cache_read_tokens > 0 || cost.cache_creation_tokens > 0);

  return (
    <span
      className="chat__cost"
      title={`${cost.prompt_tokens} in, ${cost.completion_tokens} out, over ${cost.turns} turn${
        cost.turns === 1 ? "" : "s"
      }${
        caching
          ? `. ${cost.cache_read_tokens} of the input was read from the prompt cache and ${cost.cache_creation_tokens} was written to it.`
          : ""
      }${
        cost.unreported > 0
          ? `. ${cost.unreported} of them reported no usage, so this is a floor.`
          : ""
      }`}
    >
      {cost.unreported > 0 ? "≥ " : ""}
      {formatTokens(total)} tokens
      {caching ? ` · ${cached}% cached` : ""}
    </span>
  );
}

/**
 * Folds older turns now (Phase 14); sessions also fold automatically. Hidden
 * while a turn runs.
 */
function CompactButton() {
  const session = useSessions((s) => s.detail?.session ?? null);
  const compaction = useSessions((s) => s.detail?.compaction ?? null);
  const streaming = useSessions((s) => s.streaming);
  const busy = useSessions((s) => s.busy);
  const compact = useSessions((s) => s.compact);

  if (session === null || streaming !== null) {
    return null;
  }

  return (
    <button
      type="button"
      className="link chat__compact"
      disabled={busy}
      // The promise the button has to make, on the button: this is not a
      // delete. Someone who thought it trimmed their conversation would be
      // right to never press it.
      title={
        compaction === null
          ? "Folds the older turns into a few lines of state, so replies stop paying for the whole conversation. Nothing is deleted — the transcript stays exactly as it is."
          : `Folds again, up to the last few turns. ${compaction.folded} messages are already folded. Nothing is deleted.`
      }
      onClick={() => void compact()}
    >
      Compact
    </button>
  );
}

/**
 * The oldest pending approval; more than one means other sessions are blocked,
 * shown as a count.
 */
function ApprovalQueue() {
  const pending = useApprovals((s) => s.pending);
  const first = pending.at(0);

  if (first === undefined) {
    return null;
  }
  return <ApprovalDialog request={first} queued={pending.length - 1} />;
}

export default function ChatPane() {
  const project = useProjects((s) => s.detail?.project ?? null);
  const detail = useSessions((s) => s.detail);
  const sessions = useSessions((s) => s.sessions);
  const create = useSessions((s) => s.create);

  if (project === null) {
    return null;
  }

  if (detail === null) {
    return (
      <section className="chat chat--empty">
        <h1 className="work__title">
          {sessions.length === 0 ? "No sessions yet" : "No session open"}
        </h1>
        <p className="work__body">
          {sessions.length === 0
            ? `Start a session to work in ${project.name}. Everything you send is kept in this project, and every tool call it makes is written to the audit log.`
            : "Pick a session from the sidebar to carry on where you left off."}
        </p>
        {sessions.length === 0 ? (
          <button
            type="button"
            className="button button--primary"
            onClick={() => void create(project.id)}
          >
            Add session
          </button>
        ) : null}
      </section>
    );
  }

  return (
    <section className="chat">
      <header className="chat__header">
        <h1 className="chat__title">{detail.session.title}</h1>
        <div className="chat__meta">
          <AgentBadge agentId={detail.session.agent_id} />
          <ModelBadge />
          <CostBadge />
          <CompactButton />
          <StatusLine />
        </div>
      </header>

      {project.workspace_exists ? null : (
        <p className="work__warning" role="status">
          This project's workspace folder is missing, so no tool can run in this
          session. You can still talk to the model; anything that would touch
          the filesystem is refused.
        </p>
      )}

      <MessageList />
      <ApprovalQueue />
      <GrantList />
      <Composer />
    </section>
  );
}
