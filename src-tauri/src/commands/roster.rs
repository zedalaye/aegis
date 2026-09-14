//! Roster commands (PLAN 7.14).
//!
//! Commands, never tools: a session may write a proposal but only a person's
//! press in Settings applies it, and that press is the grant.
//!
//! * [`roster_proposal`] judges `.aegis/roster/PROPOSAL.md` against the
//!   identities on file (created, skipped, refused).
//! * [`roster_apply`] creates what the preview showed, all or none, audited.

use tauri::{AppHandle, State};

use crate::agent::event::{Event, EventSink};
use crate::error::AppResult;
use crate::roster::{self, RosterApplied, RosterProposal};
use crate::state::AppState;
use crate::store::canonical_workspace;

use super::session::WindowSink;
use super::workspace::project_root;

/// The open project's roster proposal, or `None` when it has none.
///
/// `None` for no project, and for a project whose folder has gone, for the
/// reason `skill_proposals` answers an empty list there: there is no workspace
/// to hold one. Measured on every call; the file is somebody's to edit.
#[tauri::command(rename_all = "snake_case")]
pub fn roster_proposal(
    state: State<'_, AppState>,
    project_id: Option<String>,
) -> AppResult<Option<RosterProposal>> {
    let Some(id) = project_id else {
        return Ok(None);
    };
    let project = state.store().get(&id)?;
    if !project.workspace_exists {
        return Ok(None);
    }
    let root = canonical_workspace(&project.workspace_path)?;

    Ok(roster::read(
        &root,
        state.agents(),
        &state.connectors().catalog().names(),
        &state.skill_catalog(Some(&root)),
    ))
}

/// Creates the identities the open project's roster proposes.
///
/// `digest` is what the preview carried, and a file that has changed since is
/// refused: what was confirmed is what is created. Names already on file are
/// skipped and left as they are. No routine, connector or world is written.
#[tauri::command(rename_all = "snake_case")]
pub fn roster_apply(
    app: AppHandle,
    state: State<'_, AppState>,
    project_id: String,
    digest: String,
) -> AppResult<RosterApplied> {
    let root = project_root(&state, &project_id)?;

    let (applied, lines) = roster::apply(
        &root,
        state.agents(),
        &state.connectors().catalog().names(),
        &state.skill_catalog(Some(&root)),
        &digest,
        state.audit(),
        &project_id,
    )?;

    let sink = WindowSink::new(app);
    for line in lines {
        sink.emit(Event::AuditAppended(Box::new(line)));
    }
    Ok(applied)
}
