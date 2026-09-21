//! Answering a parked ask (PLAN 7.22), composed across stores.
//!
//! The three answers are three different records: *allow once* is a one-shot
//! in the grant store, *allow standing* is signed onto the routine through the
//! same door a person saving one passes, and *deny* is a line in the audit
//! log. All three take the question off the board, and the caller resumes the
//! run.

use super::*;

use crate::audit::{AuditDecision, AuditRecord, Outcome};
use crate::error::ErrorCode;
use crate::store::{ParkedAsk, RoutineDraft};

impl AppState {
    /// Records one answer to a parked ask and takes it off the board.
    ///
    /// Nothing is resumed here. Errors leave the park open: a standing answer
    /// the routine's door refuses (`E_INVALID_SETTING`), a row that offers no
    /// grant (`E_GRANT_NOT_ALLOWED`), and a park that was already answered or
    /// has expired (`E_APPROVAL_STALE`).
    pub fn answer_parked(&self, id: &str, decision: Decision) -> AppResult<ParkedAsk> {
        let ask = self.parked.get(id)?;

        // Before the park is taken: a refused door leaves the question where
        // it was rather than losing it to a failed answer.
        if decision == Decision::AllowSession {
            self.sign_answer(&ask)?;
        }

        let ask = self
            .parked
            .take(id)
            .ok_or_else(|| AppError::ParkedNotFound { id: id.to_owned() })?;

        if decision == Decision::AllowOnce {
            self.grants.allow_once(&ask.session_id, &ask.fingerprint);
        }

        tracing::info!(id, tool = %ask.tool, "a parked ask was answered");
        Ok(ask)
    }

    /// Signs an *allow standing* answer where it belongs: onto the routine,
    /// or — for a dialog that expired in a session someone opened — onto that
    /// session, which is what "allow for this session" has always meant.
    fn sign_answer(&self, ask: &ParkedAsk) -> AppResult<()> {
        let Some(grant) = ask.grant.clone() else {
            return Err(AppError::GrantNotAllowed {
                tool: ask.tool.clone(),
            });
        };

        if ask.routine_id.is_empty() {
            self.grants.insert(&ask.session_id, grant);
            return Ok(());
        }

        let routine = self.routines.get(&ask.routine_id)?;
        let agent = self.agents.get(&routine.agent_id)?;
        let workspace = self.workspace_for_project(&routine.project_id);
        let catalog = self.skill_catalog(workspace.as_deref());
        let skill = crate::skills::find(&catalog, routine.skill.trim());

        // The same door as `routine_save` (PLAN 7.13): a standing approval
        // answered from the board is still a standing approval, and it is
        // refused for the same reasons — a grant the runbook never declared,
        // a tool the identity does not hold, `world/`.
        let mut grants = routine.grants.clone();
        if !grants.contains(&grant) {
            grants.push(grant.clone());
        }
        let draft = RoutineDraft {
            name: routine.name.clone(),
            project_id: routine.project_id.clone(),
            agent_id: routine.agent_id.clone(),
            skill: routine.skill.clone(),
            schedule: routine.schedule.clone(),
            grants,
            runs_per_day: routine.runs_per_day,
            spend: routine.spend,
        };
        crate::schedule::check(
            &draft,
            &agent,
            skill,
            self.audit.witnessed(&agent.id, routine.skill.trim()),
        )?;

        let signed = self.routines.sign(&routine.id, &grant)?;
        // The routine's row changed under the panel that is not showing it.
        self.grants.insert(&ask.session_id, grant);
        tracing::info!(routine = %signed.name, "a standing approval was signed from the board");
        Ok(())
    }

    /// The audit line a refused park leaves (PLAN 7.22, *Answering*).
    ///
    /// Keyed on the call that was parked, so the refusal sits with the rest of
    /// the run it belongs to. The arguments are not repeated: they are already
    /// on the line this one answers, and the fingerprint joins the two.
    pub fn audit_parked_refusal(&self, ask: &ParkedAsk) -> AuditEntry {
        self.audit.append(&AuditRecord {
            session_id: &ask.session_id,
            agent_id: &ask.agent_id,
            turn_id: &ask.turn_id,
            call_id: &ask.call_id,
            tool: &ask.tool,
            skill: &ask.skill,
            handoff: "",
            routine: &ask.routine_id,
            decision: AuditDecision::Deny,
            policy_reason: &format!(
                "a person refused the parked call `{}` ({})",
                ask.fingerprint, ask.summary
            ),
            args: &serde_json::Value::Null,
            outcome: Outcome::Denied,
            duration_ms: 0,
            bytes_in: 0,
            bytes_out: 0,
            error_code: Some(ErrorCode::Denied),
            artifact: None,
        })
    }
}
