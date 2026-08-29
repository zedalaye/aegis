/**
 * The work area when a project is open: a session header, the transcript, and
 * the composer.
 *
 * The header shows what the turn is doing, and what is answering it, rather
 * than only what the session is called — those are the two questions a user
 * has while a reply streams. When no session is open it explains the two
 * states worth telling apart — no sessions yet, or one not chosen — since the
 * fix differs.
 */

import { useProjects } from "../../state/projects";
import { useApprovals } from "../../state/approvals";
import { useSessions } from "../../state/sessions";
import { formatTimestamp } from "../../lib/format";

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
            New session
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
          <ModelBadge />
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
