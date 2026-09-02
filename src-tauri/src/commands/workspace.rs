//! Shared-workspace commands (PLAN 7.3, Phase 11; PLAN 7.2 for the world).
//!
//! Three commands, and none of them is a way to edit the files. Reading and
//! writing `STATUS.md` or `DECISIONS.md` is what `fs_read` and `fs_write`
//! already do, under the approval gate and on the audit log; a second write
//! path around that gate is exactly the shortcut PLAN 7.1 tells this phase not
//! to take. What the UI cannot do for itself is find out whether the convention
//! is present, and lay it down once — so that is all that is here.
//!
//! The third is [`world_status`], and it is deliberately *only* a read. There
//! is no `world_scaffold` beside `workspace_scaffold`: the cabinet is a
//! convention worth laying down in an empty folder, and the constitution is
//! not. Five empty templates in a workspace with no essence are the theatre
//! PLAN 7.2 refuses — a world starts when somebody writes `world/essence.md`,
//! in their own editor or through `fs_write` under the gate, and this command
//! reports what they wrote.

use tauri::State;

use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::store::canonical_workspace;
use crate::workspace::{self, ScaffoldReport, WorkspaceLayout};
use crate::world::{self, WorldStatus};

/// Which parts of the convention exist in a project's workspace right now.
///
/// Measured on every call rather than cached: the folder belongs to the user,
/// who may well have made `.aegis/decisions/` in a terminal a minute ago, and a panel
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

/// Whether this project's workspace holds a world, and what is true of it.
///
/// The expensive measurement, on purpose: every declared source is hashed if it
/// has to be, so the panel can say *drifted* rather than *possibly drifted*.
/// That is the right cost for a panel somebody opened and the wrong one for a
/// model request, which is why the block that reaches the model reports only
/// the drift a glance can see
/// ([`world::block`](crate::world::block)).
///
/// Never fails on a folder that has no world: `present: false` and the
/// constitution's file names, which is what lets the panel say what a world is
/// without pretending this workspace has one.
#[tauri::command(rename_all = "snake_case")]
pub fn world_status(state: State<'_, AppState>, project_id: String) -> AppResult<WorldStatus> {
    let project = state.store().get(&project_id)?;
    Ok(world::status(project.workspace_path.as_ref()))
}
