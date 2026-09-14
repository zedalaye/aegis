//! Shared-workspace commands (PLAN 7.3, Phase 11; PLAN 7.2 for the world).
//!
//! None of these edits the files; that is `fs_write` under the gate (PLAN 7.1).
//! [`workspace_layout`] and [`workspace_scaffold`] measure and lay down the
//! convention; [`world_status`] only reads (there is no world scaffold);
//! [`workspace_reveal`] opens a contained path in the file manager (PLAN 7.10).
//! The explorer ([`explorer`](super::explorer)) shares [`project_root`].

use std::path::PathBuf;

use tauri::State;

use crate::error::{AppError, AppResult};
use crate::git;
use crate::reveal;
use crate::state::AppState;
use crate::store::canonical_workspace;
use crate::workspace::{self, ScaffoldReport, WorkspaceLayout};
use crate::world::{self, WorldStatus};

/// Which parts of the convention exist in a project's workspace right now.
///
/// Measured on every call; a missing folder reports everything absent.
#[tauri::command(rename_all = "snake_case")]
pub fn workspace_layout(
    state: State<'_, AppState>,
    project_id: String,
) -> AppResult<WorkspaceLayout> {
    let project = state.store().get(&project_id)?;
    Ok(workspace::layout(project.workspace_path.as_ref()))
}

/// Creates the missing directories and seed files, and versions the folder.
///
/// Explicit only: the press is the consent, for the directories and for the
/// empty `.git` (PLAN 7.11) — no commit, remote or identity. The report lists
/// what was created and kept.
///
/// `git init` runs on the project's execution host (PLAN 7.12), hence `async`.
/// The path is re-canonicalized first, catching a folder replaced by a link.
#[tauri::command(rename_all = "snake_case")]
pub async fn workspace_scaffold(
    state: State<'_, AppState>,
    project_id: String,
) -> AppResult<ScaffoldReport> {
    let project = state.store().get(&project_id)?;

    if !project.workspace_exists {
        return Err(AppError::WorkspacePath {
            path: project.workspace_path,
            reason: "the folder is not there any more".to_owned(),
        });
    }

    let root = canonical_workspace(&project.workspace_path)?;
    let report = workspace::scaffold(&root)?;
    Ok(report.versioned(git::ensure(&root, project.exec_host.as_ref()).await))
}

/// Opens a workspace path in the OS file manager (PLAN 7.10).
///
/// An empty `path` is the project folder; anything else must resolve inside it.
/// The window has no opener permission of its own.
#[tauri::command(rename_all = "snake_case")]
pub fn workspace_reveal(
    state: State<'_, AppState>,
    project_id: String,
    path: Option<String>,
) -> AppResult<()> {
    let root = project_root(&state, &project_id)?;
    let target = reveal::target(&root, path.as_deref())?;
    reveal::open(&target)
}

/// A project's workspace, canonical and present, for a command that is about
/// to resolve a path the window sent against it.
///
/// Re-canonicalized, like [`workspace_scaffold`]; shared by reveal and the
/// explorer (PLAN 7.10, 7.15).
pub(super) fn project_root(state: &AppState, project_id: &str) -> AppResult<PathBuf> {
    let project = state.store().get(project_id)?;

    if !project.workspace_exists {
        return Err(AppError::WorkspacePath {
            path: project.workspace_path,
            reason: "the folder is not there any more".to_owned(),
        });
    }

    canonical_workspace(&project.workspace_path)
}

/// Whether this project's workspace holds a world, and what is true of it.
///
/// Hashes declared sources as needed (unlike
/// [`world::block`](crate::world::block)). No world is `present: false`, not an
/// error.
#[tauri::command(rename_all = "snake_case")]
pub fn world_status(state: State<'_, AppState>, project_id: String) -> AppResult<WorldStatus> {
    let project = state.store().get(&project_id)?;
    Ok(world::status(project.workspace_path.as_ref()))
}
