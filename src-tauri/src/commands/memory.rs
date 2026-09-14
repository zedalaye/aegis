//! Memory commands (PLAN 7.3, Phase 14).
//!
//! The person's half of role memory: see, correct, forget (`COS.md` *Roles*).
//! The model has no `memory_forget`. A write from this panel is the human
//! speaking for themselves, so unlike runbooks it does not go through the gate.

use tauri::State;

use crate::error::AppResult;
use crate::state::AppState;
use crate::store::{Memory, MemoryDraft};

/// One identity's memories, most recently touched first.
///
/// `agent_id` is required, never defaulted.
#[tauri::command(rename_all = "snake_case")]
pub fn memory_list(state: State<'_, AppState>, agent_id: String) -> AppResult<Vec<Memory>> {
    state.memory_list(&agent_id)
}

/// Records a memory, or corrects one.
///
/// `None` creates; otherwise replaces, keeping the id. A duplicate text returns
/// the existing memory. Refusals: `E_INVALID_SETTING` + `error.field`.
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
/// No undo. Another identity's memory is not found
/// ([`AppError::MemoryNotFound`](crate::AppError::MemoryNotFound)).
#[tauri::command(rename_all = "snake_case")]
pub fn memory_forget(
    state: State<'_, AppState>,
    agent_id: String,
    memory_id: String,
) -> AppResult<Memory> {
    state.memory_forget(&agent_id, &memory_id)
}
