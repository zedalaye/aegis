//! Board commands (PLAN 7.3, Phase 17).
//!
//! Read-only but for one button: `STATUS.md` is corrected through a gated
//! `fs_write` (PLAN 7.6), runs are folded from `audit.jsonl` on every read,
//! and [`board_restore`] is the operator taking a run back (PLAN 7.24).

use std::path::PathBuf;

use tauri::State;

use crate::board::trace::{RunRef, RunTrace};
use crate::board::Board;
use crate::error::{AppError, AppResult};
use crate::exec_host::ExecHost;
use crate::git::checkpoint::{self, Checkpoint, Restored};
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

/// A run's checkpoint: what it changed, as a file list and a diff (PLAN 7.24).
///
/// `None` when the run has none: the workspace is not a work tree, its
/// identity could not write, or it has been pruned.
#[tauri::command(rename_all = "snake_case")]
pub async fn board_checkpoint(
    state: State<'_, AppState>,
    project_id: String,
    session_id: String,
) -> AppResult<Option<Checkpoint>> {
    let (root, host) = run_workspace(&state, &project_id, &session_id)?;
    checkpoint::read(&root, host.as_ref(), &session_id)
        .await
        .map_err(|reason| AppError::Checkpoint { reason })
}

/// Restores the `before` tree of the paths a run changed, except those changed
/// again since it ended (PLAN 7.24). The operator's act; the index, `HEAD` and
/// branches are left alone.
#[tauri::command(rename_all = "snake_case")]
pub async fn board_restore(
    state: State<'_, AppState>,
    project_id: String,
    session_id: String,
) -> AppResult<Restored> {
    let (root, host) = run_workspace(&state, &project_id, &session_id)?;
    checkpoint::restore(&root, host.as_ref(), &session_id)
        .await
        .map_err(|reason| AppError::Checkpoint { reason })
}

/// The workspace and host a run's session belongs to. A session of another
/// project is refused as not found, whatever the window sent.
fn run_workspace(
    state: &AppState,
    project_id: &str,
    session_id: &str,
) -> AppResult<(PathBuf, Option<ExecHost>)> {
    if state.sessions().project_of(session_id)? != project_id {
        return Err(AppError::SessionNotFound {
            id: session_id.to_owned(),
        });
    }
    let root = state
        .workspace_for_project(project_id)
        .ok_or_else(|| AppError::Checkpoint {
            reason: "this project's folder is not there".to_owned(),
        })?;
    Ok((root, state.exec_host_for_project(project_id)))
}
