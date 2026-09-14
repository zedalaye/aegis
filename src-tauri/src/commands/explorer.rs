//! Workspace explorer commands (PLAN 7.15).
//!
//! No command edits files (PLAN 7.5).
//!
//! * [`workspace_tree`] lists one folder.
//! * [`workspace_preview`] describes one file, with its text when it is text.
//! * [`workspace_image`] returns raw bytes for a blob URL (never `file://` or
//!   `asset:`).
//! * [`workspace_import_brief`] copies a drop into `.aegis/briefs/` ([`intake`]).
//!
//! Paths resolve inside the workspace like `workspace_reveal`. `async` keeps
//! disk reads off the main thread.

use tauri::ipc::Response;
use tauri::{AppHandle, State};

use crate::agent::event::{Event, EventSink};
use crate::error::{AppError, AppResult};
use crate::explorer::{self, FilePreview, TreeListing};
use crate::intake::{self, ImportReport};
use crate::state::AppState;

use super::session::WindowSink;
use super::workspace::project_root;

/// One folder of the open project's workspace.
///
/// `dir` omitted, or empty, is the root. `ignored` includes `.git`,
/// `node_modules` and what the ignore files name, marked, instead of counting
/// them as hidden.
#[tauri::command(rename_all = "snake_case")]
pub async fn workspace_tree(
    state: State<'_, AppState>,
    project_id: String,
    dir: Option<String>,
    ignored: Option<bool>,
) -> AppResult<TreeListing> {
    let root = project_root(&state, &project_id)?;
    explorer::list(&root, dir.as_deref(), ignored.unwrap_or(false))
}

/// One file: its name, size, type, and its text when it is text.
#[tauri::command(rename_all = "snake_case")]
pub async fn workspace_preview(
    state: State<'_, AppState>,
    project_id: String,
    path: String,
) -> AppResult<FilePreview> {
    let root = project_root(&state, &project_id)?;
    explorer::preview(&root, &path)
}

/// One image's bytes, as a binary response rather than JSON.
///
/// The type is not returned here: the preview that offered the image already
/// said what it is, and a response body is bytes and nothing else.
#[tauri::command(rename_all = "snake_case")]
pub async fn workspace_image(
    state: State<'_, AppState>,
    project_id: String,
    path: String,
) -> AppResult<Response> {
    let root = project_root(&state, &project_id)?;
    let (bytes, _mime) = explorer::image(&root, &path)?;
    Ok(Response::new(bytes))
}

/// Copies the files of one drop into the project's `.aegis/briefs/`.
///
/// `drop_id` comes from `workspace:dropped`. The workspace is checked before
/// the drop is claimed, so a missing `.aegis/briefs/` keeps it for a retry.
#[tauri::command(rename_all = "snake_case")]
pub async fn workspace_import_brief(
    app: AppHandle,
    state: State<'_, AppState>,
    project_id: String,
    drop_id: String,
) -> AppResult<ImportReport> {
    let root = project_root(&state, &project_id)?;
    let briefs = intake::briefs_dir(&root)?;
    let sources = state
        .drops()
        .take(&drop_id)
        .ok_or_else(|| AppError::BriefImport {
            reason: "that drop is no longer held; drop the files again".to_owned(),
        })?;

    let (report, lines) = intake::import(&briefs, &sources, state.audit(), &project_id, &drop_id);

    let sink = WindowSink::new(app);
    for line in lines {
        sink.emit(Event::AuditAppended(Box::new(line)));
    }
    Ok(report)
}
