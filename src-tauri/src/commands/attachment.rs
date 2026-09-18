//! Attaching images to the next message (PLAN 7.20).
//!
//! Both commands copy files into the app's attachment directory and hand back
//! ids; `session_send` takes those ids. The window never names a path and no
//! byte crosses `invoke`.
//!
//! * [`attachment_pick`] runs the file dialog from Rust, like the folder
//!   picker: the WebView holds no `dialog:` permission.
//! * [`attachment_drop`] takes a drop the OS made onto the composer, by id.

use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::attach::{self, AttachReport};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// Opens the image picker and copies what was chosen. Cancelled is an empty
/// report.
#[tauri::command(rename_all = "snake_case")]
pub async fn attachment_pick(
    app: AppHandle,
    state: State<'_, AppState>,
) -> AppResult<AttachReport> {
    let (tx, rx) = tokio::sync::oneshot::channel();

    app.dialog()
        .file()
        .set_title("Attach images")
        .add_filter("Images", &["png", "jpg", "jpeg", "gif", "webp"])
        .pick_files(move |picked| {
            let _ = tx.send(picked);
        });

    let Some(picked) = rx.await.map_err(|_| {
        tracing::error!("the image picker closed without answering");
        AppError::Internal {
            what: "the image picker closed unexpectedly",
        }
    })?
    else {
        return Ok(AttachReport::default());
    };

    let mut report = AttachReport::default();
    let mut sources = Vec::new();
    for file in picked {
        let shown = file.to_string();
        match file.into_path() {
            Ok(path) => sources.push(path),
            Err(_) => report.refused.push(attach::NotAttached {
                name: shown,
                reason: "it is not a file on this machine".to_owned(),
            }),
        }
    }

    let copied = attach::import(state.attachments(), &sources);
    report.attached = copied.attached;
    report.refused.extend(copied.refused);
    Ok(report)
}

/// Copies the image files of one drop. `drop_id` comes from
/// `workspace:dropped`; a drop onto the composer never becomes a brief.
#[tauri::command(rename_all = "snake_case")]
pub async fn attachment_drop(
    state: State<'_, AppState>,
    drop_id: String,
) -> AppResult<AttachReport> {
    let sources = state
        .drops()
        .take(&drop_id)
        .ok_or_else(|| AppError::Attach {
            reason: "that drop is no longer held; drop the files again".to_owned(),
        })?;
    Ok(attach::import(state.attachments(), &sources))
}
