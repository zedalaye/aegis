//! Memory commands (PLAN 7.3, Phase 14).
//!
//! Three commands, and between them they are the half of role memory that
//! belongs to the person rather than to the model: **see what it remembers**,
//! **correct one**, **delete one**.
//!
//! That split is the phase's one real design decision, so it is worth stating
//! here as well as in the store. `COS.md` *Roles* gives the human three jobs —
//! irreversible decisions, the quality bar, and memory correction — and the
//! tools the model holds (`memory_write`, `memory_search`) deliberately stop
//! short of all three. There is no `memory_forget` tool. An agent that could
//! quietly retire the memories it found inconvenient would have a memory
//! exactly as reliable as its judgement on its worst turn, and the record it
//! would most often delete is a correction somebody made.
//!
//! Unlike the skill commands, a *write* path from the window is right here.
//! That is not an inconsistency: a runbook changes what the agent will do and
//! so belongs behind the gate and on the audit log, while a memory typed in
//! this panel is the human speaking as themselves — the authority the gate
//! exists to serve, not something it exists to check. What goes through the
//! gate is the model writing one, which is `memory_write` and is audited like
//! every other call.

use tauri::State;

use crate::error::AppResult;
use crate::state::AppState;
use crate::store::{Memory, MemoryDraft};

/// One identity's memories, most recently touched first.
///
/// `agent_id` is required rather than defaulted. "Whose memory" is the whole
/// question this panel answers, and a list that quietly showed the built-in
/// identity's when a picker had not loaded yet would be the one wrong thing it
/// could draw.
#[tauri::command(rename_all = "snake_case")]
pub fn memory_list(state: State<'_, AppState>, agent_id: String) -> AppResult<Vec<Memory>> {
    state.memory_list(&agent_id)
}

/// Records a memory, or corrects one.
///
/// `memory_id` of `null` records a new one; otherwise it replaces that one,
/// keeping its id so nothing that refers to it is orphaned. A refused field
/// comes back as `E_INVALID_SETTING` with `error.field` — the same shape the
/// provider and identity forms already use, so no screen has to learn a second
/// vocabulary for "this input is wrong".
///
/// A new memory whose text an existing one already carries touches that one
/// instead of storing a second copy, and comes back with the existing id. A
/// panel that assumed it had created a row will therefore find it already in
/// the list, which is the truth.
#[tauri::command(rename_all = "snake_case")]
pub fn memory_save(
    state: State<'_, AppState>,
    agent_id: String,
    memory_id: Option<String>,
    draft: MemoryDraft,
) -> AppResult<Memory> {
    state.memory_save(&agent_id, memory_id.as_deref(), &draft)
}

/// Forgets one memory, and hands back what went.
///
/// Returning the record rather than nothing is what lets the panel say *what*
/// was forgotten and offer to retype it. There is no undo: the store is the
/// only copy, and a deletion the user has to confirm and can retype is a
/// simpler promise than a bin nobody empties.
///
/// A memory belonging to another identity is not found rather than refused —
/// see [`AppError::MemoryNotFound`](crate::AppError::MemoryNotFound).
#[tauri::command(rename_all = "snake_case")]
pub fn memory_forget(
    state: State<'_, AppState>,
    agent_id: String,
    memory_id: String,
) -> AppResult<Memory> {
    state.memory_forget(&agent_id, &memory_id)
}
