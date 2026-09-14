//! Approval commands (PLAN 2.1, "Approvals").
//!
//! `approval_list_pending` re-syncs a window that missed events;
//! `approval_resolve` answers, failing with `E_APPROVAL_STALE` rather than
//! silently. The grant commands make session grants visible and revocable
//! (PLAN 3.1). Nothing here runs a tool: the released turn does, through
//! [`tools::run`](crate::tools::run).

use tauri::{AppHandle, Runtime, State};

use crate::agent::event::{Event, EventSink, ToolApprovalResolved};
use crate::approval::{ApprovalRequest, Decision};
use crate::error::AppResult;
use crate::policy::Grant;
use crate::state::AppState;

use super::session::WindowSink;

/// What a session is waiting for, oldest first.
///
/// `session_id` omitted means every session's queue, which is what a window
/// showing more than one would ask for. The list is authoritative: a request
/// missing from it cannot be answered, whatever the UI still has on screen.
#[tauri::command(rename_all = "snake_case")]
pub fn approval_list_pending(
    state: State<'_, AppState>,
    session_id: Option<String>,
) -> AppResult<Vec<ApprovalRequest>> {
    Ok(state.pending_approvals(session_id.as_deref()))
}

/// Answers one approval.
///
/// Releases the parked turn. Errors: `E_GRANT_NOT_ALLOWED` (request stays
/// open, PLAN 3.1) or `E_APPROVAL_STALE` (re-sync via
/// [`approval_list_pending`]).
#[tauri::command(rename_all = "snake_case")]
pub fn approval_resolve<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    request_id: String,
    decision: Decision,
) -> AppResult<()> {
    let resolution = state.resolve_approval(&request_id, decision)?;

    // The turn emits its own `tool:approval_resolved` when it wakes, which is
    // the one that is guaranteed for every ending — including the ones nobody
    // clicked. This one is emitted here so the card closes on the click rather
    // than on a task being scheduled; the UI treats the pair as idempotent,
    // because they carry the same `request_id`.
    WindowSink::new(app).emit(Event::ToolApprovalResolved(ToolApprovalResolved {
        session_id: resolution.request.session_id.clone(),
        turn_id: resolution.request.turn_id.clone(),
        request_id: resolution.request.request_id.clone(),
        call_id: resolution.request.call_id.clone(),
        decision: resolution.decision,
        resolved_by: crate::approval::ResolvedBy::User,
    }));

    Ok(())
}

/// The `allow_session` grants a session currently holds.
///
/// Empty for a session that has only ever answered `allow_once`, which is the
/// point: allowing one call leaves nothing behind.
#[tauri::command(rename_all = "snake_case")]
pub fn approval_grants(state: State<'_, AppState>, session_id: String) -> AppResult<Vec<Grant>> {
    Ok(state.grants().list(&session_id))
}

/// Withdraws one grant.
///
/// Idempotent; the boolean says whether this call removed it.
#[tauri::command(rename_all = "snake_case")]
pub fn approval_revoke_grant(
    state: State<'_, AppState>,
    session_id: String,
    grant: Grant,
) -> AppResult<bool> {
    Ok(state.grants().revoke(&session_id, &grant))
}
