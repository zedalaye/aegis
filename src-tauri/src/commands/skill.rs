//! Skill commands (PLAN 7.3, Phase 13).
//!
//! Listings only (catalog and proposals, PLAN 7.13). Writing a runbook is an
//! editor or a gated `fs_write`; running one is a turn's job.

use tauri::State;

use crate::error::AppResult;
use crate::skills::{Skill, SkillProposal};
use crate::state::AppState;
use crate::store::canonical_workspace;

/// Every runbook the library and one project's workspace hold.
///
/// Measured each call. `None`, or a project whose folder is gone, lists the
/// library alone.
#[tauri::command(rename_all = "snake_case")]
pub fn skill_list(state: State<'_, AppState>, project_id: Option<String>) -> AppResult<Vec<Skill>> {
    let workspace = match project_id {
        Some(id) => {
            let project = state.store().get(&id)?;
            project
                .workspace_exists
                .then(|| canonical_workspace(&project.workspace_path))
                .transpose()?
        }
        None => None,
    };

    Ok(state.skill_catalog(workspace.as_deref()))
}

/// Every `PROPOSAL.md` in one project's workspace (PLAN 7.13).
///
/// Kept apart from [`skill_list`] so a proposal is never grantable. Applying
/// is a session's gated `fs_write`. Empty without a workspace.
#[tauri::command(rename_all = "snake_case")]
pub fn skill_proposals(
    state: State<'_, AppState>,
    project_id: Option<String>,
) -> AppResult<Vec<SkillProposal>> {
    let Some(id) = project_id else {
        return Ok(Vec::new());
    };
    let project = state.store().get(&id)?;
    if !project.workspace_exists {
        return Ok(Vec::new());
    }

    Ok(crate::skills::proposals(&canonical_workspace(
        &project.workspace_path,
    )?))
}
