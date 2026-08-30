/**
 * The open project's sessions, most recently active first.
 *
 * Sits under the project rail in the sidebar. It draws nothing at all when no
 * project is open: a "New session" button with nowhere to put the session is
 * a button that can only produce an error.
 *
 * From Phase 12 the button carries a choice of identity beside it. The picker
 * is here rather than in the chat pane because the choice is only ever made
 * once — a session is bound to an identity when it is created and stays bound,
 * so a control offering the choice later would be offering something that
 * cannot be done.
 */

import { useState } from "react";

import { DEFAULT_AGENT_ID, useAgents } from "../../state/agents";
import { useProjects } from "../../state/projects";
import { useSessions } from "../../state/sessions";

import SessionItem from "./SessionItem";

/**
 * Which identity the next session opens as.
 *
 * A plain `<select>`, defaulting to the built-in identity: creating a session
 * has always been one click, and a picker that forced a decision before every
 * conversation would make the common case worse to serve the rare one. Hidden
 * entirely until there is something to choose between, for the same reason.
 */
function IdentityPicker({
  value,
  onChange,
}: {
  readonly value: string;
  readonly onChange: (agentId: string) => void;
}) {
  const agents = useAgents((s) => s.agents);

  if (agents.length < 2) {
    return null;
  }

  return (
    <label className="sessions__identity">
      <span className="sessions__identitylabel">as</span>
      <select
        className="sessions__identityselect"
        value={value}
        onChange={(event) => onChange(event.target.value)}
      >
        {agents.map((agent) => (
          <option key={agent.id} value={agent.id}>
            {agent.name}
          </option>
        ))}
      </select>
    </label>
  );
}

export default function SessionList() {
  const projectId = useProjects((s) => s.detail?.project.id ?? null);
  const sessions = useSessions((s) => s.sessions);
  const openId = useSessions((s) => s.detail?.session.id ?? null);
  const busy = useSessions((s) => s.busy);
  const create = useSessions((s) => s.create);
  const open = useSessions((s) => s.open);
  const rename = useSessions((s) => s.rename);
  const remove = useSessions((s) => s.remove);
  const agents = useAgents((s) => s.agents);

  // Kept here rather than in a store: it is what the *next* click will do, not
  // a fact about anything that exists, and it is forgotten when the rail goes.
  const [asAgent, setAsAgent] = useState(DEFAULT_AGENT_ID);

  // The chosen identity can be removed from under this control — the settings
  // panel is one click away. Falling back to the built-in one keeps the button
  // working, rather than sending an id the runtime will refuse.
  const chosen = agents.some((agent) => agent.id === asAgent)
    ? asAgent
    : DEFAULT_AGENT_ID;

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

      <div className="sessions__new">
        <button
          type="button"
          className="button button--wide"
          disabled={busy}
          onClick={() => void create(projectId, chosen)}
        >
          New session
        </button>
        <IdentityPicker value={chosen} onChange={setAsAgent} />
      </div>
    </section>
  );
}
