/**
 * The open project's sessions, most recently active first.
 *
 * Sits under the project rail in the sidebar. It draws nothing at all when no
 * project is open: a "New session" button with nowhere to put the session is
 * a button that can only produce an error.
 */

import { useProjects } from "../../state/projects";
import { useSessions } from "../../state/sessions";

import SessionItem from "./SessionItem";

export default function SessionList() {
  const projectId = useProjects((s) => s.detail?.project.id ?? null);
  const sessions = useSessions((s) => s.sessions);
  const openId = useSessions((s) => s.detail?.session.id ?? null);
  const busy = useSessions((s) => s.busy);
  const create = useSessions((s) => s.create);
  const open = useSessions((s) => s.open);
  const rename = useSessions((s) => s.rename);
  const remove = useSessions((s) => s.remove);

  if (projectId === null) {
    return null;
  }

  return (
    <section className="sessions" aria-label="Sessions">
      <h2 className="sidebar__heading">Sessions</h2>

      {sessions.length === 0 ? (
        <p className="sidebar__empty">No sessions in this project yet.</p>
      ) : (
        <ul className="sessions__list">
          {sessions.map((session) => (
            <SessionItem
              key={session.id}
              session={session}
              open={session.id === openId}
              onOpen={() => void open(session.id)}
              onRename={(title) => void rename(session.id, title)}
              onRemove={() => void remove(session.id)}
            />
          ))}
        </ul>
      )}

      <button
        type="button"
        className="button button--wide"
        disabled={busy}
        onClick={() => void create(projectId)}
      >
        New session
      </button>
    </section>
  );
}
