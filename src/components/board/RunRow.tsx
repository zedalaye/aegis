/**
 * One run in the list under the board.
 *
 * One line: who, cost, and why when it went wrong; the rest behind a
 * disclosure. The status is the run's own report, even with refused calls
 * inside (shown as a count).
 */

import type { Run, RunKind, RunStatus } from "../../ipc/bindings";
import {
  formatCost,
  formatDuration,
  formatTimestamp,
  formatTimeOfDay,
} from "../../lib/format";

import RunCheckpoint from "./RunCheckpoint";

/** What each kind of run is called. */
const KIND: Record<RunKind, string> = {
  handoff: "delegation",
  routine: "routine",
  skill: "runbook",
  session: "conversation",
};

/** How each ending reads. */
const STATUS: Record<RunStatus, string> = {
  done: "done",
  blocked: "blocked",
  needs_you: "needs you",
  failed: "failed",
  ran: "ran",
};

/**
 * The sessions whose checkpoints this run has (PLAN 7.24): a routine's one,
 * or each session a delegation opened. Conversations and runbooks run
 * attended and are not checkpointed.
 */
function checkpointed(run: Run): readonly string[] {
  switch (run.run.kind) {
    case "routine":
      return [run.run.session_id];
    case "handoff":
      return run.sessions;
    default:
      return [];
  }
}

export default function RunRow({
  projectId,
  run,
  open,
  onToggle,
  onOpenSession,
}: {
  readonly projectId: string;
  readonly run: Run;
  /** Whether this run's replay is the one on screen. */
  readonly open: boolean;
  readonly onToggle: () => void;
  readonly onOpenSession: (sessionId: string) => void;
}) {
  const wall =
    run.started_at.length > 0 && run.ended_at.length > 0
      ? Date.parse(run.ended_at) - Date.parse(run.started_at)
      : Number.NaN;

  return (
    <li className={`run run--${run.status}`}>
      <button
        type="button"
        className="run__head"
        aria-expanded={open}
        onClick={onToggle}
      >
        <span className="run__label">{run.label}</span>
        <span className={`run__status run__status--${run.status}`}>
          {STATUS[run.status]}
        </span>
        <span className="run__kind">{KIND[run.run.kind]}</span>
        <span className="run__cost">{formatCost(run.cost)}</span>
        {run.ended_at.length > 0 ? (
          <time
            className="run__time"
            dateTime={run.ended_at}
            title={formatTimestamp(run.ended_at)}
          >
            {formatTimeOfDay(run.ended_at)}
          </time>
        ) : null}
      </button>

      {run.reason.length > 0 ? (
        <p className="run__reason">{run.reason}</p>
      ) : null}

      {open ? (
        <dl className="run__facts">
          <dt>Who</dt>
          <dd>
            {run.agents.length === 0 ? "—" : run.agents.join(", ")}
            {run.skill.length > 0 ? (
              <>
                {" "}
                following <code>{run.skill}</code>
              </>
            ) : null}
          </dd>

          <dt>Calls</dt>
          <dd>
            {run.calls} — {run.asked} asked, {run.denied} refused, {run.failed}{" "}
            failed
            {run.tools.length > 0 ? (
              <span className="run__tools">
                {run.tools.map((tally) => (
                  <span className="run__tool" key={tally.tool}>
                    {tally.tool} ×{tally.calls}
                  </span>
                ))}
              </span>
            ) : null}
          </dd>

          <dt>Spent</dt>
          <dd>
            {formatCost(run.cost)}
            {run.cost.unreported > 0 ? (
              <span className="run__caveat">
                {" "}
                — {run.cost.unreported} turn
                {run.cost.unreported === 1 ? "" : "s"} the provider reported
                nothing for
              </span>
            ) : null}
            <span className="run__caveat">
              {" "}
              · {formatDuration(run.tool_ms)} in tools
              {Number.isFinite(wall) ? (
                <> of {formatDuration(wall)} elapsed</>
              ) : null}
            </span>
          </dd>

          {run.artefacts.length > 0 ? (
            <>
              <dt>Left behind</dt>
              <dd>
                <ul className="run__artefacts">
                  {run.artefacts.map((path) => (
                    <li className="run__artefact" key={path}>
                      <code>{path}</code>
                    </li>
                  ))}
                </ul>
              </dd>
            </>
          ) : null}

          {checkpointed(run).length > 0 ? (
            <>
              <dt>Changed</dt>
              <dd>
                {checkpointed(run).map((sessionId) => (
                  <RunCheckpoint
                    key={sessionId}
                    projectId={projectId}
                    sessionId={sessionId}
                    // A delegation lists the delegating session too, which
                    // is a conversation and never has a checkpoint.
                    explainAbsence={run.run.kind === "routine"}
                  />
                ))}
              </dd>
            </>
          ) : null}

          {run.sessions.length > 0 ? (
            <>
              <dt>Sessions</dt>
              <dd className="run__sessions">
                {run.sessions.map((sessionId) => (
                  <button
                    type="button"
                    className="run__session"
                    key={sessionId}
                    onClick={() => onOpenSession(sessionId)}
                  >
                    open the transcript
                  </button>
                ))}
              </dd>
            </>
          ) : null}
        </dl>
      ) : null}
    </li>
  );
}
