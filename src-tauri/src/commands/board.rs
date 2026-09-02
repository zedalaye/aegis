//! Board commands (PLAN 7.3, Phase 17).
//!
//! Two read-only commands, and there is deliberately no third. Nothing here
//! writes `.aegis/status/STATUS.md`: the board's file half is the user's file, and the
//! way a status is corrected is an ordinary `fs_write` through the approval
//! gate, on the audit log, exactly like a decision (PLAN 7.6, *Authoring*). A
//! `board_write` would be a second path into the workspace that skipped both.
//!
//! Nor is there a command that writes a run. A run is folded out of
//! `audit.jsonl` every time it is asked for; there is no store of runs to
//! corrupt, and a UI able to edit one would be a UI able to edit the record of
//! what the agent did.

use tauri::State;

use crate::board::trace::{RunRef, RunTrace};
use crate::board::Board;
use crate::error::AppResult;
use crate::state::AppState;

/// The open project's board: attention, in flight, blocked, and its runs.
///
/// Measured on every call rather than cached. Half of what it reports is
/// live — what is running, what a dialog is waiting on, which clock stopped
/// itself — and a board served out of a cache would be a board that is wrong
/// exactly when somebody is looking at it to find out what changed.
#[tauri::command(rename_all = "snake_case")]
pub fn board_read(state: State<'_, AppState>, project_id: String) -> AppResult<Board> {
    Ok(state.board(&project_id))
}

/// One run, and the audit lines it is replayed from, oldest first.
///
/// The reference comes back from the board unchanged, which is what makes this
/// safe to expose: it names a grouping of lines that are already in the log,
/// not a path, a session or anything the WebView could widen. A run the window
/// is holding but the log has scrolled past is [`AppError::RunNotFound`], which
/// the panel answers by refetching the board.
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
