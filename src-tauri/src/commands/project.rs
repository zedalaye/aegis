//! Project commands (PLAN 2.1, "Projects").
//!
//! A project is a workspace folder plus a name; from Phase 3 it is also the
//! root every path check is measured against, which is why the path is
//! canonicalized here, once, on the way in. Everything downstream compares
//! against a resolved path rather than against whatever string the UI happened
//! to hold.
//!
//! The folder picker runs in Rust. `capabilities/main.json` grants the WebView
//! no `dialog:` permission, so the only way to open one is this command — the
//! same shape as the window commands, and for the same reason: the privileged
//! operation stays on the runtime side of the boundary.

use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::store::{self, Project, ProjectDetail};

/// Opens the native folder picker and returns the chosen workspace.
///
/// Returns `None` when the user cancels — a cancelled dialog is an ordinary
/// outcome, not a failure, and the UI should do nothing rather than show an
/// error. The path comes back canonicalized, so the string the UI then hands
/// to [`project_create`] is already the one that will be stored.
///
/// The picker's callback fires on whichever thread the platform's dialog runs
/// on, so the result is handed back over a oneshot channel rather than by
/// blocking: `blocking_pick_folder` deadlocks when it lands on the main
/// thread, and which thread a command runs on is not something this code
/// should have to depend on.
#[tauri::command(rename_all = "snake_case")]
pub async fn project_pick_workspace(app: AppHandle) -> AppResult<Option<String>> {
    let (tx, rx) = tokio::sync::oneshot::channel();

    app.dialog()
        .file()
        .set_title("Choose a workspace folder")
        .pick_folder(move |picked| {
            // The receiver is only dropped if the command was cancelled; the
            // user's choice then has nowhere to go, which is fine.
            let _ = tx.send(picked);
        });

    let Some(picked) = rx.await.map_err(|_| {
        tracing::error!("the folder picker closed without answering");
        AppError::Internal {
            what: "the folder picker closed unexpectedly",
        }
    })?
    else {
        tracing::debug!("workspace picker cancelled");
        return Ok(None);
    };

    // Kept for the error message: `into_path` consumes the choice, and a
    // failure there is exactly the case where naming it is worth something.
    let shown = picked.to_string();
    let path = picked.into_path().map_err(|err| {
        tracing::warn!(%err, "the picker returned a location that is not a local path");
        AppError::WorkspacePath {
            path: shown,
            reason: "that location is not a folder on this machine".to_owned(),
        }
    })?;

    let canonical = store::canonical_workspace(&path.to_string_lossy())?;

    tracing::info!("workspace picked");
    Ok(Some(canonical.to_string_lossy().into_owned()))
}

/// Registers a workspace folder as a project.
///
/// An empty `name` falls back to the folder's own name. Adding a folder that
/// is already a project returns the existing project rather than a duplicate.
#[tauri::command(rename_all = "snake_case")]
pub fn project_create(
    state: State<'_, AppState>,
    name: String,
    path: String,
) -> AppResult<Project> {
    let workspace = store::canonical_workspace(&path)?;
    state.store().create(&name, &workspace)
}

/// Every project, most recently opened first.
#[tauri::command(rename_all = "snake_case")]
pub fn project_list(state: State<'_, AppState>) -> AppResult<Vec<Project>> {
    Ok(state.store().list())
}

/// Opens a project and marks it as the most recent one.
///
/// A project whose folder has since been moved or unmounted still opens; the
/// detail carries `workspace_exists: false` so the UI can say so instead of
/// pretending the project is gone.
#[tauri::command(rename_all = "snake_case")]
pub fn project_open(state: State<'_, AppState>, project_id: String) -> AppResult<ProjectDetail> {
    state.store().open(&project_id)
}

/// Forgets a project. The workspace folder on disk is never touched.
#[tauri::command(rename_all = "snake_case")]
pub fn project_delete(state: State<'_, AppState>, project_id: String) -> AppResult<()> {
    state.store().delete(&project_id)
}
