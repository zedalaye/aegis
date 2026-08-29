//! Approval commands (PLAN 2.1, "Approvals").
//!
//! Four commands, and the shape of the pair that matter is the point of the
//! module. `approval_list_pending` is a *re-sync*, not the primary path: a
//! dialog normally arrives as a `tool:approval_required` event, and this is
//! what a window that was closed, reloaded or opened on another session calls
//! to find out what it missed. `approval_resolve` is the answer, and it fails
//! loudly rather than silently — `E_APPROVAL_STALE` on a request that expired
//! or was already answered, so the UI refetches instead of closing a card on
//! the belief that it approved something.
//!
//! The grant commands are the visible-and-revocable half of PLAN 3.1. A
//! session grant is the only thing in this application that makes future tool
//! calls run without asking, so it must be possible to see the whole list of
//! them and to take one back. Settings will host that list from Phase 8; the
//! commands are here because a grant is created by an approval and dies with
//! the session, not by anything settings owns.
//!
//! Nothing here decides anything. `approval_resolve` records the grant and
//! releases the turn; the *call* is run by the turn loop, under the same
//! [`tools::run`](crate::tools::run) every other call goes through, because a
//! command that could execute a tool would be a second way into the machine
//! that policy does not gate.

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
/// Three outcomes, and only the first is a success:
///
/// * the turn is released and runs — or does not run — the call it was parked
///   on, and `tool:approval_resolved` says which;
/// * `E_GRANT_NOT_ALLOWED` when `allow_session` is answered on a row that
///   offers no grant (PLAN 3.1). The request stays open: the user has not yet
///   answered the question they were actually asked;
/// * `E_APPROVAL_STALE` when nothing is waiting on that id — it expired, it
///   was already answered, or its turn was cancelled. The UI re-syncs through
///   [`approval_list_pending`] rather than branching on it.
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
/// Idempotent, and deliberately not an error when there was nothing to
/// withdraw: the user's intent — "stop allowing this" — is satisfied either
/// way, and a revoke that failed because someone else had already revoked it
/// would be a dialog about nothing. The boolean says whether this call is the
/// one that removed it.
#[tauri::command(rename_all = "snake_case")]
pub fn approval_revoke_grant(
    state: State<'_, AppState>,
    session_id: String,
    grant: Grant,
) -> AppResult<bool> {
    Ok(state.grants().revoke(&session_id, &grant))
}
