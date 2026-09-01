/**
 * What is on a clock (PLAN 7.3, Phase 16).
 *
 * A section of Settings beside Identities and Skills, because a routine is the
 * third side of the same question: a skill is what may be run, an identity is
 * who may run it, and a routine is when it runs with nobody watching.
 *
 * Each row says the four things that decide whether you would leave it running:
 * when it fires, what it fires, how it went last time, and what it is allowed to
 * do while nobody is there. The last of those is spelled out rather than
 * counted — "2 approvals" is not an answer to "may this thing write to my repo
 * at four in the morning".
 *
 * A row that cannot fire says so. That sentence is measured by the runtime on
 * every list, never remembered here: a clock drawn as running when its runbook
 * was un-granted is the one wrong thing this panel could show.
 */

import type { Grant, Routine } from "../../ipc/bindings";
import { useRoutines } from "../../state/routines";
import { useAgents } from "../../state/agents";

import RoutineForm from "./RoutineForm";

/** What each standing approval covers, in the fewest words that bound it. */
export function grantLabel(grant: Grant): string {
  switch (grant.kind) {
    case "fs_write":
      return "write files inside the workspace, except under .git/";
    case "fs_read_large":
      return "read files over 1 MB";
    case "shell":
      return `run ${grant.program}`;
    case "screen_capture":
      return "capture the primary display";
    case "memory_write":
      return "record memories as this identity";
    case "handoff_delegate":
      return "hand briefs to other identities";
    case "connector":
      // The whole tool name, not the connector's: a grant covers what the
      // dialog named and nothing the server adds afterwards.
      return `call ${grant.tool}`;
  }
}

/** How a schedule reads on a row. Mirrors `Schedule::label` in Rust. */
export function scheduleLabel(routine: Routine): string {
  const schedule = routine.schedule;
  switch (schedule.kind) {
    case "every":
      return schedule.minutes % 60 === 0 && schedule.minutes >= 60
        ? schedule.minutes === 60
          ? "every hour"
          : `every ${schedule.minutes / 60} hours`
        : `every ${schedule.minutes} minutes`;
    case "daily_at":
      return `daily at ${String(schedule.hour).padStart(2, "0")}:${String(
        schedule.minute,
      ).padStart(2, "0")}`;
    case "on_change":
      return `when ${schedule.dir} changes`;
  }
}

/** How the last run reads, or `null` for a routine that has never run. */
function LastRun({ routine }: { readonly routine: Routine }) {
  const last = routine.last;
  if (last === null) {
    return <p className="routine__last routine__last--never">Has not run yet.</p>;
  }

  const when = new Date(last.at).toLocaleString();
  return (
    <p className={`routine__last routine__last--${last.outcome}`}>
      <span className="routine__outcome">{last.outcome.replace("_", " ")}</span>
      <span className="routine__when">{when}</span>
      {last.detail.length === 0 ? null : (
        <span className="routine__detail">{last.detail}</span>
      )}
    </p>
  );
}

/** One routine. */
function Row({ routine }: { readonly routine: Routine }) {
  const busy = useRoutines((s) => s.busy);
  const startEdit = useRoutines((s) => s.startEdit);
  const remove = useRoutines((s) => s.remove);
  const setPaused = useRoutines((s) => s.setPaused);
  const runNow = useRoutines((s) => s.runNow);
  const agents = useAgents((s) => s.agents);

  const identity =
    agents.find((agent) => agent.id === routine.agent_id)?.name ??
    "an identity that is gone";

  return (
    <li className={`routine${routine.paused ? " routine--paused" : ""}`}>
      <div className="routine__head">
        <span className="routine__name">{routine.name}</span>
        {routine.paused ? (
          <span className="routine__badge" title={routine.paused_reason}>
            paused
          </span>
        ) : null}
        <span className="routine__actions">
          <button
            type="button"
            className="link"
            // Said on the control, because it is the surprising half: the
            // button is not a rehearsal, it is the thing itself.
            title="Runs it now, exactly as the clock would — unattended, so anything it was not signed for is refused rather than put to you."
            onClick={() => void runNow(routine.id)}
            disabled={busy}
          >
            Run now
          </button>
          <button
            type="button"
            className="link"
            onClick={() => void setPaused(routine.id, !routine.paused)}
            disabled={busy}
          >
            {routine.paused ? "Resume" : "Pause"}
          </button>
          <button
            type="button"
            className="link"
            onClick={() => startEdit(routine.id)}
            disabled={busy}
          >
            Edit
          </button>
          <button
            type="button"
            className="link"
            title="Removes the clock. The sessions its runs opened stay where they are."
            onClick={() => void remove(routine.id)}
            disabled={busy}
          >
            Remove
          </button>
        </span>
      </div>

      <p className="routine__what">
        <code>{routine.skill}</code> as <strong>{identity}</strong>,{" "}
        {scheduleLabel(routine)} · {routine.runs_today}/{routine.runs_per_day}{" "}
        runs today
      </p>

      {routine.grants.length === 0 ? (
        <p className="routine__grants routine__grants--none">
          Signed for nothing: its runs may read, and a write or a command is
          refused.
        </p>
      ) : (
        <p className="routine__grants">
          May{" "}
          {routine.grants.map((grant, at) => (
            <span key={`${grant.kind}-${at}`} className="routine__grant">
              {grantLabel(grant)}
            </span>
          ))}{" "}
          while nobody is watching.
        </p>
      )}

      {routine.paused && routine.paused_reason.length > 0 ? (
        <p className="routine__problem">{routine.paused_reason}</p>
      ) : null}
      {routine.problem === null ? null : (
        <p className="routine__problem">{routine.problem}</p>
      )}

      <LastRun routine={routine} />
    </li>
  );
}

export default function RoutineList() {
  const routines = useRoutines((s) => s.routines);
  const status = useRoutines((s) => s.status);
  const busy = useRoutines((s) => s.busy);
  const editing = useRoutines((s) => s.editing);
  const startNew = useRoutines((s) => s.startNew);

  if (status === "loading" && routines.length === 0) {
    return <p className="settings__note">Loading routines…</p>;
  }

  const open =
    editing === null
      ? null
      : (routines.find((routine) => routine.id === editing) ?? null);

  return (
    <>
      <p className="settings__note">
        A routine fires one runbook, as one identity, on a clock or when a
        folder changes — and it runs whether or not this window is open. It can
        only name a skill that identity was granted and has already run under
        your eye at least once: putting a procedure on a clock is the last step
        of writing it down, not the first.
      </p>

      {routines.length === 0 && editing === null ? (
        <p className="settings__note">
          Nothing is scheduled. Nothing runs on its own until something is.
        </p>
      ) : (
        <ul className="routine__list">
          {routines.map((routine) => (
            <Row key={routine.id} routine={routine} />
          ))}
        </ul>
      )}

      {editing === null ? (
        <button
          type="button"
          className="button"
          onClick={startNew}
          disabled={busy}
        >
          New routine
        </button>
      ) : (
        <RoutineForm key={editing} editing={open} />
      )}
    </>
  );
}
