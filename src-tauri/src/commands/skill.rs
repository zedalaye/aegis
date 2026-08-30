//! Skill commands (PLAN 7.3, Phase 13).
//!
//! One command, and the shape of it is the point: the catalog can be *listed*
//! and nothing else. There is no `skill_create`, no editor, and no command
//! that runs one.
//!
//! A runbook is a `SKILL.md` in the user's library or in their workspace, and
//! writing one is what an editor is for — or, inside a session, an ordinary
//! `fs_write` through the approval gate, exactly like a decision or a status
//! (PLAN 7.3, Phase 11). A privileged write path from the WebView into the
//! skill library would be a second way to change what the agent will do,
//! reachable without the gate and absent from the audit log.
//!
//! And running one is a thing a *turn* does, on the model's initiative, under
//! the identity's allow-list. A command that ran a skill would be a second
//! agent loop with no transcript, no approvals and no audit line — the shape
//! `AGENTS.md` names as the wrong one.

use tauri::State;

use crate::error::AppResult;
use crate::skills::Skill;
use crate::state::AppState;
use crate::store::canonical_workspace;

/// Every runbook the library and one project's workspace hold.
///
/// `project_id` is optional because Settings is reachable with nothing open:
/// `None` lists the library alone, which is the honest answer when there is no
/// workspace to look in.
///
/// Measured on every call rather than cached, like the workspace layout and
/// for the same reason: `skills/` is an ordinary directory in somebody's own
/// folder, and a panel that trusted a cache would be confidently wrong about a
/// runbook they edited a minute ago in their editor.
///
/// A project whose folder has gone lists the library alone rather than
/// failing. The workspace badge already reports the missing folder, and a
/// second error saying the same thing in different words is not more
/// information.
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
