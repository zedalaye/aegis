//! Board commands (PLAN 7.3, Phase 17).
//!
//! Read-only: `STATUS.md` is corrected through a gated `fs_write` (PLAN 7.6),
//! and runs are folded from `audit.jsonl` on every read.

use tauri::State;

use crate::board::trace::{RunRef, RunTrace};
use crate::board::Board;
use crate::error::AppResult;
use crate::state::AppState;

/// The open project's board: attention, in flight, blocked, and its runs.
///
/// Measured on every call.
#[tauri::command(rename_all = "snake_case")]
pub fn board_read(state: State<'_, AppState>, project_id: String) -> AppResult<Board> {
    Ok(state.board(&project_id))
}

/// One run, and the audit lines it is replayed from, oldest first.
///
/// `run` names lines already in the log, nothing the WebView could widen. A
/// run scrolled out of the tail is [`AppError::RunNotFound`].
///
/// [`AppError::RunNotFound`]: crate::error::AppError::RunNotFound
#[tauri::command(rename_all = "snake_case")]
pub fn board_trace(
    state: State<'_, AppState>,
    project_id: String,
    run: RunRef,
) -> AppResult<RunTrace> {
    state.run_trace(&project_id, &run)
}
