//! Shared-workspace commands (PLAN 7.3, Phase 11).
//!
//! Two commands, and neither of them is a way to edit the files. Reading and
//! writing `STATUS.md` or `DECISIONS.md` is what `fs_read` and `fs_write`
//! already do, under the approval gate and on the audit log; a second write
//! path around that gate is exactly the shortcut PLAN 7.1 tells this phase not
//! to take. What the UI cannot do for itself is find out whether the convention
//! is present, and lay it down once — so that is all that is here.

use tauri::State;

use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::store::canonical_workspace;
use crate::workspace::{self, ScaffoldReport, WorkspaceLayout};

/// Which parts of the convention exist in a project's workspace right now.
///
/// Measured on every call rather than cached: the folder belongs to the user,
/// who may well have made `decisions/` in a terminal a minute ago, and a panel
/// that is confidently wrong about someone's own directory is worse than no
/// panel. A project whose folder is missing reports everything absent — the
/// same answer a folder that was never scaffolded gives, which is the truth in
/// both cases.
#[tauri::command(rename_all = "snake_case")]
pub fn workspace_layout(
    state: State<'_, AppState>,
    project_id: String,
) -> AppResult<WorkspaceLayout> {
    let project = state.store().get(&project_id)?;
    Ok(workspace::layout(project.workspace_path.as_ref()))
}

/// Creates the missing directories and seed files, and nothing else.
///
/// Explicit, never automatic. A workspace is a folder the user already owns —
/// often a repository with its own layout — and four directories appearing in
/// it because they pointed an app at it is not a thing to do on their behalf.
/// The report names what was created and what was left alone, so the answer to
/// "did it touch my files" is on screen rather than in a promise.
///
/// The path is re-canonicalized here rather than trusted from the store. The
/// stored path was canonical when it was registered, and this is the one
/// command that then writes through it: a folder that has since been replaced
/// by a symlink is worth catching at the door, and the check also gives the
/// missing-folder case a message that says which folder.
#[tauri::command(rename_all = "snake_case")]
pub fn workspace_scaffold(
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
    workspace::scaffold(&root)
}
