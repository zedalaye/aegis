/**
 * The identities a session can be opened as (PLAN 7.3, Phase 12).
 *
 * A section of the settings panel rather than a surface of its own: an identity
 * is a fact about the application, like the provider, and not about any project
 * — the same reason Settings is reachable with nothing open.
 *
 * Each row says the three things that decide whether you would pick it: what
 * it is for, what it can touch, and which runbooks it may follow. The tools are
 * spelled out rather than counted, because "3 tools" is not an answer to "may
 * this thing write to my repo", and the skills are spelled out for the same
 * reason.
 */

import type { Agent } from "../../ipc/bindings";
import { useAgents } from "../../state/agents";

import AgentForm from "./AgentForm";

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
    </>
  );
}
