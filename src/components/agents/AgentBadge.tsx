/**
 * Which identity a session runs as (PLAN 7.3, Phase 12).
 *
 * Separate from the model badge: the identity is fixed per session, the model
 * is not. The built-in identity draws nothing.
 */

import { DEFAULT_AGENT_ID, useAgents } from "../../state/agents";

export default function AgentBadge({
  agentId,
}: {
  /** The identity the session is bound to. */
  readonly agentId: string;
}) {
  const agent = useAgents((s) =>
    s.agents.find((candidate) => candidate.id === agentId),
  );

  if (agentId === DEFAULT_AGENT_ID) {
    return null;
  }

  // The identity was deleted out from under the list, or the list has not
  // arrived yet. Saying so is better than drawing nothing: a session bound to
  // an identity nothing can resolve holds no tools at all, and the user should
  // find that out here rather than from a refusal.
  if (agent === undefined) {
    return (
      <span
        className="agentbadge agentbadge--unknown"
        title="This session names an identity that is not on file. It can talk, but it holds no tools."
      >
        unknown identity
      </span>
    );
  }

  return (
    <span
      className="agentbadge"
      title={
        agent.tools.length === 0
          ? `${agent.role}. No tools: this session cannot touch the machine.`
          : `${agent.role}. Tools: ${agent.tools.join(", ")}.`
      }
    >
      {agent.name}
    </span>
  );
}
