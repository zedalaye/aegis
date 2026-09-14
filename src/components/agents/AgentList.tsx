/**
 * The identities a session can be opened as (PLAN 7.3, Phase 12).
 *
 * In Settings, since identities are app-wide. Rows show role, and tools and
 * skills spelled out rather than counted.
 */

import type { Agent } from "../../ipc/bindings";
import { useAgents } from "../../state/agents";

import AgentForm from "./AgentForm";
import RosterPanel from "./RosterPanel";

/** What an identity may touch, in the words the row can afford. */
function Tools({ agent }: { readonly agent: Agent }) {
  if (agent.tools.length === 0) {
    return (
      <p className="agent__tools agent__tools--none">
        No tools. It can read the conversation and answer; it cannot touch the
        machine.
      </p>
    );
  }

  return (
    <p className="agent__tools">
      {agent.tools.map((tool) => (
        <code key={tool}>{tool}</code>
      ))}
    </p>
  );
}

/** One identity. */
function Row({ agent }: { readonly agent: Agent }) {
  const busy = useAgents((s) => s.busy);
  const startEdit = useAgents((s) => s.startEdit);
  const startClone = useAgents((s) => s.startClone);
  const remove = useAgents((s) => s.remove);

  return (
    <li className="agent">
      <div className="agent__head">
        <span className="agent__name">{agent.name}</span>
        {agent.builtin ? (
          <>
            <span
              className="agent__badge"
              title="The identity a session gets when none is chosen. It holds every tool and no skill — it is what Aegis was before either allow-list existed, which is why it cannot be edited or removed."
            >
              built in
            </span>
            <span className="agent__actions">
              <button
                type="button"
                className="link"
                title="Opens a new identity shaped like this one. The built-in identity cannot be edited; a copy of it can, which is the usual way to make a narrower one."
                onClick={() => startClone(agent)}
                disabled={busy}
              >
                Duplicate
              </button>
            </span>
          </>
        ) : (
          <span className="agent__actions">
            <button
              type="button"
              className="link"
              onClick={() => startEdit(agent.id)}
              disabled={busy}
            >
              Edit
            </button>
            <button
              type="button"
              className="link"
              // `COS.md`: clone a role without cloning its rotten memory. The
              // copy is a new identity, so it starts with none of the first
              // one's — and none of its record either.
              title="Opens a new identity with the same perimeter: role, instructions, tools, runbooks, budget. Its memories are not copied — they belong to the identity that learned them."
              onClick={() => startClone(agent)}
              disabled={busy}
            >
              Duplicate
            </button>
            <button
              type="button"
              className="link"
              // Said on the control: the refusal that follows when sessions are
              // still bound is a real one, and knowing the rule before pressing
              // is better than reading it in a banner afterwards.
              title="Removes the identity. Refused while any session still runs as it."
              onClick={() => void remove(agent.id)}
              disabled={busy}
            >
              Remove
            </button>
          </span>
        )}
      </div>

      {agent.role.length === 0 ? null : (
        <p className="agent__role">{agent.role}</p>
      )}
      <Tools agent={agent} />
      {agent.skills.length === 0 ? null : (
        <p className="agent__skills">
          May run{" "}
          {agent.skills.map((skill) => (
            <code key={skill}>{skill}</code>
          ))}
        </p>
      )}
    </li>
  );
}

export default function AgentList() {
  const agents = useAgents((s) => s.agents);
  const status = useAgents((s) => s.status);
  const busy = useAgents((s) => s.busy);
  const editing = useAgents((s) => s.editing);
  const startNew = useAgents((s) => s.startNew);

  if (status === "loading" && agents.length === 0) {
    return <p className="settings__note">Loading identities…</p>;
  }

  const open =
    editing === null
      ? null
      : (agents.find((agent) => agent.id === editing) ?? null);

  return (
    <>
      <p className="settings__note">
        An identity is a name, what it is for, the tools it may use, and the
        runbooks it may follow. A session is opened as one and stays as one —
        the transcript is the record of what that identity did. Editing an
        identity reaches its sessions on their next turn.
      </p>

      <ul className="agent__list">
        {agents.map((agent) => (
          <Row key={agent.id} agent={agent} />
        ))}
      </ul>

      {/*
        The empty state names the two acts founding starts with and performs
        neither (PLAN 7.14). The built-in row is a constant and gets no skill,
        so the founder is a copy of it — a row somebody made.
      */}
      {agents.length > 1 || editing !== null ? null : (
        <p className="settings__note">
          Only the built-in Assistant so far. To found a cabinet,{" "}
          <strong>Duplicate</strong> it, tick <code>cabinet.found</code> under
          Skills on the copy, and ask a session opened as the copy for one. It
          writes a roster proposal into the open project, and nobody exists
          until you apply that here.
        </p>
      )}

      {editing === null ? (
        <button
          type="button"
          className="button"
          onClick={startNew}
          disabled={busy}
        >
          New identity
        </button>
      ) : (
        <AgentForm key={editing} editing={open} />
      )}

      <RosterPanel />
    </>
  );
}
