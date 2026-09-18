//! Project eval commands (PLAN 7.18).
//!
//! Listings only. Signed evals and proposals are two commands, so a proposal
//! is never shown as runnable or grantable. Applying one is a session's gated
//! `fs_write`; running one is `jev_eval`.

use tauri::State;

use crate::agent::decision::eval::{self, EvalEntry, EvalProposal};
use crate::error::AppResult;
use crate::state::AppState;
use crate::store::canonical_workspace;

/// The workspace of a project, or `None` when its folder is gone.
fn workspace_of(state: &AppState, project_id: &str) -> AppResult<Option<std::path::PathBuf>> {
    let project = state.store().get(project_id)?;
    project
        .workspace_exists
        .then(|| canonical_workspace(&project.workspace_path))
        .transpose()
}

/// Every signed `eval.yml` in one project's workspace.
#[tauri::command(rename_all = "snake_case")]
pub fn eval_list(state: State<'_, AppState>, project_id: String) -> AppResult<Vec<EvalEntry>> {
    Ok(workspace_of(&state, &project_id)?
        .map(|root| eval::list(&root))
        .unwrap_or_default())
}

/// Every `PROPOSAL.yml` in one project's workspace.
#[tauri::command(rename_all = "snake_case")]
pub fn eval_proposals(
    state: State<'_, AppState>,
    project_id: String,
) -> AppResult<Vec<EvalProposal>> {
    Ok(workspace_of(&state, &project_id)?
        .map(|root| eval::proposals(&root))
        .unwrap_or_default())
}
