/**
 * The status board, and the runs underneath it (PLAN 7.3, Phase 17).
 *
 * Takes over the work area. The board's three columns come from `STATUS.md`
 * and runtime state; below, runs newest first, each opening to its raw audit
 * lines. Read-only: corrections go through `STATUS.md`.
 */

import { useEffect } from "react";

import type { BoardItem } from "../../ipc/bindings";
import { sameRun, useBoard } from "../../state/board";
import { useProjects } from "../../state/projects";
import { useSessions } from "../../state/sessions";
import { formatCost } from "../../lib/format";

import AuditRow from "../audit/AuditRow";
import BoardColumn from "./BoardColumn";
import RunRow from "./RunRow";

/** What each column is for, in the one line that goes under its heading. */
const NOTES = {
  attention: "Somebody has to do something.",
  inFlight: "Running right now.",
  blocked: "Stopped short, and not waiting on a person.",
} as const;

/** One run's audit lines, oldest first, using the audit drawer's rows. */
function Trace() {
  const trace = useBoard((s) => s.trace);
  const tracing = useBoard((s) => s.tracing);
  const close = useBoard((s) => s.closeTrace);

  if (tracing) {
    return <p className="board__lede">Reading the log…</p>;
  }
  if (trace === null) {
    return null;
  }

  return (
    <section className="trace" aria-label={`Replay of ${trace.run.label}`}>
      <header className="trace__header">
        <h3 className="trace__title">
          {trace.run.label}
          <span className="trace__count">
            {trace.entries.length} call
            {trace.entries.length === 1 ? "" : "s"}
          </span>
        </h3>
        <button type="button" className="button" onClick={close}>
          Close
        </button>
      </header>

      {trace.entries.length === 0 ? (
        <p className="board__lede">
          This run reached a model but called no tool, so the log has nothing
          about it. What it cost is on the row above.
        </p>
      ) : (
        <ul className="trace__list">
          {trace.entries.map((entry) => (
            <AuditRow
              key={`${entry.ts}-${entry.call_id}`}
              entry={entry}
              // A delegation spans sessions, so which one a line was made in is
              // part of reading it.
              showSession={trace.run.sessions.length > 1}
            />
          ))}
        </ul>
      )}
    </section>
  );
}

export default function BoardPanel() {
  const close = useBoard((s) => s.closePanel);
  const board = useBoard((s) => s.board);
  const status = useBoard((s) => s.status);
  const trace = useBoard((s) => s.trace);
  const refresh = useBoard((s) => s.refresh);
  const toggleTrace = useBoard((s) => s.toggleTrace);
  const projectId = useProjects((s) => s.detail?.project.id ?? null);
  const followProject = useBoard((s) => s.followProject);
  const openSession = useSessions((s) => s.open);

  // The panel follows the open project the way the drawer follows the open
  // session: a board belongs to one folder, and switching project while it is
  // up should redraw it rather than leave the previous one on screen.
  useEffect(() => {
    void followProject(projectId);
  }, [projectId, followProject]);

  const openTranscript = (sessionId: string) => {
    void openSession(sessionId);
    close();
  };

  const traceItem = (item: BoardItem) => {
    if (item.run !== null) {
      void toggleTrace(item.run);
    }
  };

  return (
    <section className="board" aria-labelledby="board-title">
      <header className="board__header">
        <h1 className="board__title" id="board-title">
          Board
        </h1>
        <div className="board__controls">
          <button
            type="button"
            className="button"
            disabled={status === "loading"}
            onClick={() => void refresh()}
          >
            {status === "loading" ? "Reading…" : "Refresh"}
          </button>
          <button type="button" className="button" onClick={close}>
            Close
          </button>
        </div>
      </header>

      {board === null ? (
        <p className="board__lede">
          {status === "loading"
            ? "Reading the board…"
            : "Open a project to see its board."}
        </p>
      ) : (
        <>
          <p className="board__lede">
            {board.status_path.length > 0 ? (
              <>
                What is true right now, from <code>{board.status_path}</code>{" "}
                and from what this process can see for itself. Editing the file
                is how the first half changes; nothing in this window writes it.
              </>
            ) : (
              <>
                This workspace has no <code>status/STATUS.md</code>, so the
                board is only what this process can see for itself.{" "}
                <em>Set up shared files</em> in the sidebar lays down the
                convention.
              </>
            )}
          </p>

          <div className="board__columns">
            <BoardColumn
              title="Attention"
              note={NOTES.attention}
              items={board.attention}
              onOpenSession={openTranscript}
              onTrace={traceItem}
            />
            <BoardColumn
              title="In flight"
              note={NOTES.inFlight}
              items={board.in_flight}
              onOpenSession={openTranscript}
              onTrace={traceItem}
            />
            <BoardColumn
              title="Blocked"
              note={NOTES.blocked}
              items={board.blocked}
              onOpenSession={openTranscript}
              onTrace={traceItem}
            />
          </div>

          <h2 className="board__section">
            Runs
            <span className="board__total">
              this project has spent {formatCost(board.cost)}
            </span>
          </h2>

          {board.runs.length === 0 ? (
            <p className="board__lede">
              Nothing has run in this project yet. A run appears here the moment
              a turn ends — a conversation, a runbook, a delegation or a clock —
              whether it worked or not.
            </p>
          ) : (
            <ul className="board__runs">
              {board.runs.map((run) => (
                <RunRow
                  key={`${run.run.kind}-${run.run.id}-${run.run.session_id}`}
                  run={run}
                  open={trace !== null && sameRun(trace.run.run, run.run)}
                  onToggle={() => void toggleTrace(run.run)}
                  onOpenSession={openTranscript}
                />
              ))}
            </ul>
          )}

          <Trace />

          <p className="board__note">
            The runs are folded out of the tail of <code>audit.jsonl</code>, so
            a run old enough to have scrolled out of that window is in the file
            and not here. What each turn spent is on its session, beside the
            transcript — a turn that called no tool costs tokens too, which is
            why the count does not come from the log.
          </p>
        </>
      )}
    </section>
  );
}
