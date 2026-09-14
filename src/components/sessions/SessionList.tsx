/**
 * The open project's sessions, most recently active first.
 *
 * Hidden when no project is open. The heading's "+" creates a session, or
 * offers an identity menu when there are several (binding is permanent).
 */

import { DEFAULT_AGENT_ID, useAgents } from "../../state/agents";
import { useProjects } from "../../state/projects";
import { useSessions } from "../../state/sessions";

import RailAdd from "../layout/RailAdd";
import Section from "../layout/Section";

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
  const agents = useAgents((s) => s.agents);

  if (projectId === null) {
    return null;
  }

  return (
    <Section
      id="sessions"
      title="Sessions"
      className="sessions"
      badge={
        sessions.length === 0 ? null : (
          <span className="rail__count">{sessions.length}</span>
        )
      }
      action={
        <RailAdd
          label="Add session"
          disabled={busy}
          onClick={() => void create(projectId, DEFAULT_AGENT_ID)}
          items={agents.map((agent) => {
            const hint =
              agent.role.length > 0
                ? agent.role
                : agent.builtin
                  ? "built in"
                  : "";
            return {
              id: agent.id,
              label: agent.name,
              ...(hint === "" ? {} : { hint }),
              onSelect: () => {
                void create(projectId, agent.id);
              },
            };
          })}
        />
      }
    >
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
    </Section>
  );
}
