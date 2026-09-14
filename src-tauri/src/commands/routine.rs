//! Routines: what is on a clock (PLAN 7.3, Phase 16).
//!
//! List, save, delete, pause, run now. No command sends a routine a message,
//! grants anything, or skips the door: **Run now** takes the clock's unattended
//! path.

use tauri::{AppHandle, State};

use crate::error::AppResult;
use crate::schedule::{self, runner};
use crate::skills;
use crate::state::AppState;
use crate::store::{Routine, RoutineDraft};

/// Every routine, each carrying whatever is wrong with it right now.
///
/// `problem` is measured on each call, never stored.
#[tauri::command]
pub fn routine_list(state: State<'_, AppState>) -> Vec<Routine> {
    state.routine_list()
}

/// Creates a routine, or replaces one.
///
/// `None` creates; an update keeps id, today's ledger and (unless the schedule
/// changed) its cycle. Enforces the PLAN 7.13 door; refusals are
/// `E_INVALID_SETTING` with `error.field`.
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
/// The sessions its runs opened are kept.
#[tauri::command(rename_all = "snake_case")]
pub fn routine_delete(state: State<'_, AppState>, routine_id: String) -> AppResult<()> {
    state.routines().delete(&routine_id)
}

/// Stops or restarts a routine's clock.
///
/// Resuming re-arms (no catch-up run) and clears the pause reason.
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
/// Returns once started, unattended exactly like the clock; progress arrives as
/// events. Rejects when it cannot run at all (folder gone, skill un-granted,
/// budget spent, already running).
#[tauri::command(rename_all = "snake_case")]
pub fn routine_run_now(
    app: AppHandle,
    state: State<'_, AppState>,
    routine_id: String,
) -> AppResult<()> {
    runner::run_now(&app, &state, &routine_id)
}
