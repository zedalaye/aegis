//! Routines: what is on a clock (PLAN 7.3, Phase 16).
//!
//! Five commands, and between them they are the whole surface a person has on
//! the scheduler: list, save, delete, pause, run now. What is deliberately
//! *not* here is as much of the phase as what is.
//!
//! There is no command that sends a routine a message, because a routine has no
//! message: it names a runbook, and the run's opening is written by the runtime
//! from that name (`schedule::opening`). There is no command that grants
//! anything: a routine's standing approvals are checked against the runbook's
//! declared tools and the identity's allow-list, both of which are edited
//! elsewhere. And there is none that skips the door — **Run now** takes exactly
//! the path the clock takes, unattended and all, because a button that behaved
//! better than the schedule would be a button that proves nothing.

use tauri::{AppHandle, State};

use crate::error::AppResult;
use crate::schedule::{self, runner};
use crate::skills;
use crate::state::AppState;
use crate::store::{Routine, RoutineDraft};

/// Every routine, each carrying whatever is wrong with it right now.
///
/// The `problem` on a row is measured, never stored: a skill that was
/// un-granted, a folder that was unplugged and an identity that was deleted are
/// facts about this moment, and the panel draws them beside the routine rather
/// than pretending the clock is fine.
#[tauri::command]
pub fn routine_list(state: State<'_, AppState>) -> Vec<Routine> {
    state.routine_list()
}

/// Creates a routine, or replaces one.
///
/// `routine_id` of `None` creates; otherwise that routine is updated, keeping
/// its id, its ledger for today and — unless the schedule itself changed — its
/// place in the cycle.
///
/// This is where the door of PLAN 7.13 is enforced: the skill has to be live,
/// granted to the identity, and already carried to a `skill_return` by it at
/// least once. A refusal rejects with `E_INVALID_SETTING` and an `error.field`
/// naming the input, the same shape the identity and provider forms use, so the
/// message lands beside the thing that has to change.
#[tauri::command(rename_all = "snake_case")]
pub fn routine_save(
    state: State<'_, AppState>,
    routine_id: Option<String>,
    draft: RoutineDraft,
) -> AppResult<Routine> {
    // Resolved against the project the draft names rather than whatever is
    // open: a routine's runbook may be a workspace one, and "which
    // `inbox.triage`" is answered by the folder it will run in.
    let agent = state.agents().get(&draft.agent_id)?;
    let workspace = state.workspace_for_project(&draft.project_id);
    let catalog = state.skill_catalog(workspace.as_deref());
    let skill = skills::find(&catalog, draft.skill.trim());
    let witnessed = state.audit().witnessed(&agent.id, draft.skill.trim());

    schedule::check(&draft, &agent, skill, witnessed)?;

    let saved = match routine_id {
        Some(id) => state.routines().update(&id, &draft)?,
        None => state.routines().create(&draft)?,
    };
    // A routine that watches a folder starts from what is in it *now*, so a
    // file dropped in a second after saving is a change. Looking on the first
    // tick instead would mean the first thing you do to test it is the one
    // thing it cannot see.
    state.arm_watch(&saved);
    Ok(state.routine_with_problem(saved))
}

/// Deletes a routine.
///
/// The sessions its runs opened are not touched. They are transcripts of things
/// that happened, and each one still says which routine fired it and what it
/// was asked to run — a record should not stop explaining itself because the
/// clock was taken off the wall.
#[tauri::command(rename_all = "snake_case")]
pub fn routine_delete(state: State<'_, AppState>, routine_id: String) -> AppResult<()> {
    state.routines().delete(&routine_id)
}

/// Stops or restarts a routine's clock.
///
/// Un-pausing re-arms it, so a routine that was stopped for a fortnight does
/// not immediately fire for a window nobody was there for. It also clears the
/// reason, including one the scheduler wrote itself after two silent runs:
/// restarting a paused routine is a person saying they have looked.
#[tauri::command(rename_all = "snake_case")]
pub fn routine_set_paused(
    state: State<'_, AppState>,
    routine_id: String,
    paused: bool,
) -> AppResult<Routine> {
    let updated = state.routines().set_paused(&routine_id, paused)?;
    Ok(state.routine_with_problem(updated))
}

/// Fires a routine now.
///
/// Returns as soon as the run is started — the session opens, the row updates
/// and the transcript fills through the ordinary `session:updated`,
/// `routine:updated` and `turn:*` events. What it does *not* do is behave
/// differently from the clock: the run is unattended, so a call the routine was
/// not signed for is refused here exactly as it would be at four in the
/// morning, which is the only way to find that out before it happens.
///
/// Rejects when the routine could not run at all — a folder that is gone, a
/// skill that was un-granted, a budget that is spent, a run already going — so
/// the button can say why nothing happened.
#[tauri::command(rename_all = "snake_case")]
pub fn routine_run_now(
    app: AppHandle,
    state: State<'_, AppState>,
    routine_id: String,
) -> AppResult<()> {
    runner::run_now(&app, &state, &routine_id)
}
