//! Shared-workspace commands (PLAN 7.3, Phase 11; PLAN 7.2 for the world).
//!
//! Four commands, and none of them is a way to edit the files. Reading and
//! writing `STATUS.md` or `DECISIONS.md` is what `fs_read` and `fs_write`
//! already do, under the approval gate and on the audit log; a second write
//! path around that gate is exactly the shortcut PLAN 7.1 tells this phase not
//! to take. What the UI cannot do for itself is find out whether the convention
//! is present, and lay it down once — so that is two of them.
//!
//! The third is [`world_status`], and it is deliberately *only* a read. There
//! is no `world_scaffold` beside `workspace_scaffold`: the cabinet is a
//! convention worth laying down in an empty folder, and the constitution is
//! not. Five empty templates in a workspace with no essence are the theatre
//! PLAN 7.2 refuses — a world starts when somebody writes `world/essence.md`,
//! in their own editor or through `fs_write` under the gate, and this command
//! reports what they wrote.
//!
//! The fourth is [`workspace_reveal`] (PLAN 7.10). It opens a contained path
//! in the OS file manager. The WebView never opens `file://` and never gains
//! an opener permission; the argument is this project's workspace, or a path
//! already checked to sit inside it.
//!
//! Seeing the files, rather than revealing the folder, is the explorer
//! (PLAN 7.15) in [`explorer`](super::explorer). It resolves its paths through
//! the same [`project_root`] and the same containment.

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

/// Creates the missing directories and seed files, and versions the folder.
///
/// Explicit, never automatic. A workspace is a folder the user already owns —
/// often a repository with its own layout — and four directories appearing in
/// it because they pointed an app at it is not a thing to do on their behalf.
/// The report names what was created and what was left alone, so the answer to
/// "did it touch my files" is on screen rather than in a promise.
///
/// This is also the one command that may leave a `.git` in somebody's folder
/// (PLAN 7.11), and for the same reason it may leave five directories there:
/// the press is the consent. `project_create` never does — picking a folder is
/// not consent to mutate it. What it leaves is an empty repository and nothing
/// else: no commit, no remote, no identity, then or ever.
///
/// The two halves meet here rather than in [`workspace::scaffold`] because this
/// is the layer that holds a *project*: which `git` may write in that folder is
/// the project's execution host (PLAN 7.12), and a Windows `git init` on a
/// distribution's tree is the wrong git. `async` for the same reason — a WSL
/// distribution is asked whether it can see the folder before anything runs
/// there, and a virtual machine that is still coming up takes seconds to
/// answer.
///
/// The path is re-canonicalized here rather than trusted from the store. The
/// stored path was canonical when it was registered, and this is the one
/// command that then writes through it: a folder that has since been replaced
/// by a symlink is worth catching at the door, and the check also gives the
/// missing-folder case a message that says which folder.
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
/// `path` omitted, or empty, is the project folder — the title-bar button.
/// Anything else has to resolve inside that folder; a path that climbs out,
/// or that only looked contained, is refused rather than opened. The window
/// has no `fs:` / `shell:` / opener permission, so this command is the only
/// way a click reaches Explorer, Finder, or the desktop's folder handler.
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
/// Re-canonicalized rather than trusted from the store, for the reason
/// [`workspace_scaffold`] gives: a folder replaced by a link since it was
/// registered is caught at the door. Shared by reveal (PLAN 7.10) and the
/// explorer (PLAN 7.15), which are the two surfaces that take a path from the
/// window at all.
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
