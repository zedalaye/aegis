//! Audit commands (PLAN 2.1, "Settings and audit").
//!
//! Read-only: lines are written only by [`tools::run`](crate::tools::run).

use tauri::State;

use crate::audit::AuditEntry;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// Default number of entries when the caller does not say.
const DEFAULT_LIMIT: usize = 100;

/// The most recent audit entries, newest first.
///
/// Optionally for one session; `limit` is clamped, and zero returns nothing.
#[tauri::command(rename_all = "snake_case")]
pub fn audit_tail(
    state: State<'_, AppState>,
    limit: Option<usize>,
    session_id: Option<String>,
) -> AppResult<Vec<AuditEntry>> {
    state
        .audit()
        .tail(limit.unwrap_or(DEFAULT_LIMIT), session_id.as_deref())
        .map_err(|err| {
            tracing::error!(%err, "could not read the audit log");
            AppError::Audit {
                action: "read",
                source: err,
            }
        })
}

/// Where the log lives on disk.
///
/// Returned even before the first call creates the file.
#[tauri::command(rename_all = "snake_case")]
pub fn audit_log_path(state: State<'_, AppState>) -> AppResult<String> {
    Ok(state.audit().path().display().to_string())
}
