//! Project commands (PLAN 2.1, "Projects").
//!
//! Workspace paths are canonicalized once, on the way in. The folder picker
//! runs in Rust: the WebView has no `dialog:` permission.

use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::error::{AppError, AppResult};
use crate::exec_host::{self, ExecHost, ExecHostOption};
use crate::state::AppState;
use crate::store::{self, Project, ProjectDetail};

/// Opens the native folder picker and returns the chosen workspace.
///
/// `None` when cancelled; otherwise canonical. Uses a oneshot channel because
/// `blocking_pick_folder` deadlocks on the main thread.
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
/// A missing folder still opens, with `workspace_exists: false`.
#[tauri::command(rename_all = "snake_case")]
pub fn project_open(state: State<'_, AppState>, project_id: String) -> AppResult<ProjectDetail> {
    let mut detail = state.store().open(&project_id)?;

    // The store cannot fill this itself: a session's `state` is a fact about
    // the turn registry, which only `AppState` can see (PLAN 2.1).
    detail.sessions = state.session_list(&project_id);
    Ok(detail)
}

/// Every execution host this machine can offer right now (PLAN 7.12).
///
/// This computer, then `wsl.exe -l -q`'s distributions, queried each call.
#[tauri::command(rename_all = "snake_case")]
pub async fn project_list_exec_hosts() -> AppResult<Vec<ExecHostOption>> {
    Ok(exec_host::options().await)
}

/// Says where this project's commands run, or puts them back on this computer.
///
/// Explicit, never inferred from the path. Refused when the build is not
/// Windows, the distribution is not installed (installed ones are listed), or
/// the workspace has no path in it (unless the folder is gone).
#[tauri::command(rename_all = "snake_case")]
pub async fn project_set_exec_host(
    state: State<'_, AppState>,
    project_id: String,
    host: Option<ExecHost>,
) -> AppResult<Project> {
    let project = state.store().get(&project_id)?;

    if let Some(ExecHost::Wsl { distro }) = &host {
        if !exec_host::supported() {
            return Err(AppError::ExecHost {
                reason: "WSL is a Windows feature, and this is not a Windows build".to_owned(),
            });
        }

        let installed = exec_host::installed().await;
        if !installed.iter().any(|name| name == distro) {
            return Err(AppError::ExecHost {
                reason: if installed.is_empty() {
                    format!("`{distro}` is not installed, and neither is any other distribution")
                } else {
                    format!(
                        "`{distro}` is not installed. This machine has {}",
                        installed.join(", ")
                    )
                },
            });
        }

        if project.workspace_exists {
            exec_host::linux_path(distro, project.workspace_path.as_ref())
                .map_err(|reason| AppError::ExecHost { reason })?;
        }
    }

    state.store().set_exec_host(&project_id, host)
}

/// Forgets a project, and its sessions, routines and parked asks with it.
///
/// The folder is never touched. Sessions (turns cancelled first) and routines
/// cascade, since neither can outlive the project. The project goes first, so a
/// later failure leaves orphans rather than a half-deleted project.
#[tauri::command(rename_all = "snake_case")]
pub fn project_delete(state: State<'_, AppState>, project_id: String) -> AppResult<()> {
    for session in state.session_list(&project_id) {
        state.turns().forget(&session.id);
    }

    state.store().delete(&project_id)?;

    if let Err(err) = state.sessions().delete_for_project(&project_id) {
        tracing::warn!(%err, project_id, "the project is gone but its sessions remain");
    }
    if let Err(err) = state.routines().delete_for_project(&project_id) {
        tracing::warn!(%err, project_id, "the project is gone but its routines remain");
    }
    // The runs those parks would resume are gone with the sessions (PLAN 7.22).
    state.parked().forget_project(&project_id);
    Ok(())
}
