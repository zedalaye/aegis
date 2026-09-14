//! Roster commands (PLAN 7.14).
//!
//! Two commands, and the second is the only place in this application an
//! identity is created from a file. Neither is a tool: the registry has no
//! `agent_create`, `agent_update` or `roster_apply`, so a session can write a
//! roster proposal — an ordinary `fs_write`, under the gate — and cannot apply
//! one. Applying is a person pressing a button in Settings, and that press is
//! the grant.
//!
//! * [`roster_proposal`] reads the open project's `.aegis/roster/PROPOSAL.md`
//!   and judges it against the identities on file: what would be created, what
//!   is skipped because the name exists, and what would be refused.
//! * [`roster_apply`] creates the new identities with the allow-lists that
//!   preview showed — every one, or none — and puts each on the audit log.

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
