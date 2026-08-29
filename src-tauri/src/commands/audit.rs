//! Audit commands (PLAN 2.1, "Settings and audit").
//!
//! Two read-only commands over the log. There is deliberately no command that
//! writes one, clears one or edits one: audit lines are produced by
//! [`tools::run`](crate::tools::run) as a side effect of a tool call, and a
//! WebView that could append to the log — or empty it — would be a WebView
//! that could forge or erase the record of what the agent did.
//!
//! The drawer that renders these lands in Phase 10; the commands are here
//! because the log they read is written from Phase 4 on, and a log with no way
//! to read it is a log nobody checks.

use tauri::State;

use crate::audit::AuditEntry;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// Default number of entries when the caller does not say.
const DEFAULT_LIMIT: usize = 100;

/// The most recent audit entries, newest first.
///
/// `session_id` narrows it to one session — what the drawer shows beside a
/// transcript. `limit` is clamped by the log itself; a `limit` of zero returns
/// nothing rather than everything, because a UI that has just been resized to
/// zero rows should ask for zero rows.
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
/// Shown in the UI so a user can open the file themselves — the log is plain
/// JSONL and is meant to be readable without Aegis running. The path is
/// returned even when nothing has been written yet: the first tool call
/// creates the file, and telling the user where it *will* be is more useful
/// than an error.
#[tauri::command(rename_all = "snake_case")]
pub fn audit_log_path(state: State<'_, AppState>) -> AppResult<String> {
    Ok(state.audit().path().display().to_string())
}
