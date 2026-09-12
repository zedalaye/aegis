//! Workspace explorer commands (PLAN 7.15).
//!
//! Four commands, and three of them only read. None of them is a way to edit a
//! file: writing `DECISIONS.md` or a `SKILL.md` stays `fs_write`, under the
//! gate, on the audit log. A save path from this window would be the second
//! write around that gate PLAN 7.5 refuses by name.
//!
//! * [`workspace_tree`] lists one folder of the open project.
//! * [`workspace_preview`] describes one file, with its text when it is text.
//! * [`workspace_image`] hands an image over as raw bytes, which the window
//!   turns into a blob URL of its own. Never a `file://`, and not the `asset:`
//!   protocol either: that scope is the capture directory and nothing else.
//! * [`workspace_import_brief`] is the one write, and it is the operator's —
//!   the files the OS just dropped on the window, copied into
//!   `.aegis/briefs/`. See [`intake`].
//!
//! Every path argument is resolved inside the open project's canonical
//! workspace, through the same door `workspace_reveal` uses. Arbitrary paths
//! from the WebView are refused; the capabilities file gains nothing.
//!
//! `async` so a large folder or a sixteen-megabyte image is read off the main
//! thread: a synchronous Tauri command runs on it, and the window would freeze
//! while the disk answered.

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
/// `drop_id` is what `workspace:dropped` carried. The paths behind it were
/// recorded by the runtime when the OS handed them over, so the window names a
/// drop and never a path.
///
/// The workspace is checked before the drop is claimed, and that order is the
/// point: a folder with no `.aegis/briefs/` refuses without spending the drop,
/// so the operator can set up the shared files and add what they dropped
/// without dropping it again.
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
