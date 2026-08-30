//! Identity commands (PLAN 7.3, Phase 12).
//!
//! Four thin commands over [`AgentStore`](crate::store::AgentStore). There is
//! deliberately no fifth that *runs* as an identity: an identity is bound when
//! a session is created (`session_create`), and everything after that goes
//! through the turn loop the WebView already has. A command that could switch
//! the identity of a session mid-conversation would leave a transcript whose
//! history was made by someone else — `fs_write` calls in the record of an
//! identity that was never allowed to make one.
//!
//! Nor is there a command that grants a tool to a running session. The
//! allow-list is part of the identity, edited on the identity, and read at the
//! start of every turn; a per-session override would be a second place a grant
//! lives, and the two would disagree.

use tauri::State;

use crate::error::AppResult;
use crate::state::AppState;
use crate::store::{Agent, AgentDraft};

/// Every identity: the built-in one first, then the rest by name.
#[tauri::command]
pub fn agent_list(state: State<'_, AppState>) -> AppResult<Vec<Agent>> {
    Ok(state.agent_list())
}

/// Creates an identity.
///
/// Rejects with `E_INVALID_SETTING` and a `field` when a value cannot be used,
/// so the form marks the input rather than raising a banner over itself.
#[tauri::command(rename_all = "snake_case")]
pub fn agent_create(state: State<'_, AppState>, draft: AgentDraft) -> AppResult<Agent> {
    state.agents().create(&draft)
}

/// Replaces an identity's fields, keeping its id.
///
/// The id is kept so the sessions bound to it stay bound: correcting what a
/// "reviewer" is should reach the sessions already running as one, on their
/// next turn. The built-in identity is refused.
#[tauri::command(rename_all = "snake_case")]
pub fn agent_update(
    state: State<'_, AppState>,
    agent_id: String,
    draft: AgentDraft,
) -> AppResult<Agent> {
    state.agents().update(&agent_id, &draft)
}

/// Deletes an identity.
///
/// Refused while sessions still run as it, and refused outright for the
/// built-in one. Neither is a check the WebView is trusted to have made: the
/// panel hides the controls, and this is what enforces them.
#[tauri::command(rename_all = "snake_case")]
pub fn agent_delete(state: State<'_, AppState>, agent_id: String) -> AppResult<()> {
    state.delete_agent(&agent_id)
}
