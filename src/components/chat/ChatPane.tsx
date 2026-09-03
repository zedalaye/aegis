/**
 * The work area when a project is open: a session header, the transcript, and
 * the composer.
 *
 * The header shows what the turn is doing, who is doing it and what is
 * answering, rather than only what the session is called — those are the
 * questions a user has while a reply streams. When no session is open it explains the two
 * states worth telling apart — no sessions yet, or one not chosen — since the
 * fix differs.
 */

import { useProjects } from "../../state/projects";
import { useApprovals } from "../../state/approvals";
import { useSessions } from "../../state/sessions";
import { cacheShare, formatTimestamp, formatTokens } from "../../lib/format";

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
 * What this conversation has spent (PLAN 7.3, Phase 17).
 *
 * In the header beside the model, because that is where the question is asked:
 * a person wondering what a long session is costing is looking at the session.
 * The board is where the same number is asked *about* something — a run, a
 * routine, the whole project.
 *
 * Absent rather than "0 tokens" until a turn has been charged, and the phrasing
 * says "at least" when some provider reported no usage at all, because a total
 * that read as exact when it is a floor would be worse than none.
 *
 * The cached share sits in the badge rather than only in the tooltip because
 * it is not a detail of the total, it is what the total *means*: the same
 * number of tokens costs about a tenth as much when it came out of the cache,
 * and a session whose share has collapsed is a session that has started paying
 * full price to re-send itself. Hidden when the provider does not cache at
 * all, so a percentage never appears where it could only ever read zero.
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
 * Folds this session's older turns into state, on demand (PLAN 7.3, Phase 14).
 *
 * In the header rather than beside the composer: it is about the session, not
 * about the message being typed, and it is not part of sending one. Long
 * sessions fold themselves once the transcript has grown expensive; this is for
 * the times you would rather it happened now — before asking for something
 * long, say.
 *
 * Hidden while a turn runs. A fold mid-turn would change what the next round
 * carries, halfway through the reasoning the model is already doing, and the
 * runtime folds at the top of a turn for exactly that reason.
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
 * The approval the user is being asked about, if any.
 *
 * One at a time, oldest first. The turn runs its calls sequentially and parks
 * on each in turn, so a second prompt only exists when a *second session* is
 * also blocked — and the count on the card says so rather than stacking two
 * dialogs over each other.
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
