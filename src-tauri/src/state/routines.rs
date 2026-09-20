//! Routines, the board and run traces, composed across stores (Phase 16,
//! Phase 17).

use super::*;

impl AppState {
    /// Which routines are running right now.
    pub fn scheduler(&self) -> &Scheduler {
        &self.scheduler
    }

    /// Every routine, with whatever currently stops it from firing.
    pub fn routine_list(&self) -> Vec<Routine> {
        self.routines
            .list()
            .into_iter()
            .map(|mut routine| {
                routine.problem = self.routine_problem(&routine);
                routine
            })
            .collect()
    }

    /// Records where a watched folder stands when its routine is saved, so a
    /// file dropped right after fires on the next tick. Best effort: a missing
    /// folder is learned later ([`schedule::learning`](crate::schedule::learning)).
    pub fn arm_watch(&self, routine: &Routine) {
        let crate::store::Schedule::OnChange { dir } = &routine.schedule else {
            return;
        };
        let Some(workspace) = self.workspace_for_project(&routine.project_id) else {
            return;
        };
        let Ok(resolved) = crate::policy::path::resolve(&workspace, dir) else {
            return;
        };
        if !resolved.inside {
            return;
        }

        if let Some(newest) = crate::schedule::newest_change(&resolved.path) {
            self.routines.mark_seen(&routine.id, &newest);
        }
    }

    /// One routine with its problem measured, as a command hands it back.
    pub fn routine_with_problem(&self, mut routine: Routine) -> Routine {
        routine.problem = self.routine_problem(&routine);
        routine
    }

    /// Why this routine cannot fire, or `None`. The panel and the scheduler read
    /// the same answer.
    pub fn routine_problem(&self, routine: &Routine) -> Option<String> {
        let agent = self.agents.get(&routine.agent_id).ok();
        let workspace = self.workspace_for_project(&routine.project_id);
        let catalog = self.skill_catalog(workspace.as_deref());
        let skill = crate::skills::find(&catalog, &routine.skill).cloned();

        crate::schedule::inspect(
            routine,
            agent.as_ref(),
            workspace.as_deref(),
            skill.as_ref(),
            self.routines.runs_today_for_agent(&routine.agent_id),
        )
    }

    /// The project's board (Phase 17), composed from `STATUS.md`, the session,
    /// turn, approval and routine stores and the audit log — all measured now.
    pub fn board(&self, project_id: &str) -> board::Board {
        let sessions = self.session_list(project_id);
        let routines: Vec<Routine> = self
            .routine_list()
            .into_iter()
            .filter(|routine| routine.project_id == project_id)
            .collect();

        // Only this project's dialogs, by its sessions.
        let approvals: Vec<ApprovalRequest> = self
            .pending_approvals(None)
            .into_iter()
            .filter(|request| {
                sessions
                    .iter()
                    .any(|session| session.id == request.session_id)
            })
            .collect();

        let status = self
            .workspace_for_project(project_id)
            .and_then(|root| crate::workspace::status(&root))
            .map(|(path, text)| (path.display().to_string(), text));

        let runs = trace::fold(&self.audit_window(), &self.ledger(&sessions));

        board::assemble(board::Facts {
            project_id,
            status: status
                .as_ref()
                .map(|(path, text)| (path.as_str(), text.as_str())),
            sessions: &sessions,
            routines: &routines,
            approvals: &approvals,
            runs,
        })
    }

    /// One run and the audit lines it replays from, oldest first (PLAN 7.2,
    /// row 10). Folded again rather than cached.
    pub fn run_trace(&self, project_id: &str, run: &trace::RunRef) -> AppResult<RunTrace> {
        let sessions = self.session_list(project_id);
        let ledger = self.ledger(&sessions);
        let window = self.audit_window();

        let folded = trace::fold(&window, &ledger)
            .into_iter()
            .find(|folded| &folded.run == run)
            .ok_or_else(|| AppError::RunNotFound { id: run.id.clone() })?;

        let known: Vec<&str> = ledger
            .iter()
            .map(|session| session.session_id.as_str())
            .collect();
        let mut entries: Vec<AuditEntry> = window
            .into_iter()
            .filter(|entry| known.contains(&entry.session_id.as_str()))
            .filter(|entry| &trace::RunRef::of(entry) == run)
            .collect();
        // Oldest first: a replay is read forwards, unlike the drawer, which is
        // a tail and is read backwards.
        entries.sort_by(|left, right| left.ts.cmp(&right.ts));

        Ok(RunTrace {
            run: folded,
            entries,
        })
    }

    /// The audit window a board is folded from. A read failure is an empty
    /// window; the drawer reports it.
    pub(super) fn audit_window(&self) -> Vec<AuditEntry> {
        self.audit.tail(AUDIT_WINDOW, None).unwrap_or_else(|err| {
            tracing::warn!(%err, "the board could not read the audit log");
            Vec::new()
        })
    }

    /// What the fold is allowed to see, and what each session spent.
    pub(super) fn ledger(&self, sessions: &[SessionSummary]) -> Vec<trace::SessionLedger> {
        sessions
            .iter()
            .map(|session| trace::SessionLedger {
                session_id: session.id.clone(),
                title: session.title.clone(),
                routine: session
                    .scheduled
                    .as_ref()
                    .map(|scheduled| scheduled.routine_name.clone())
                    .unwrap_or_default(),
                handoff: session
                    .delegated
                    .as_ref()
                    .map(|delegated| delegated.handoff_id.clone())
                    .unwrap_or_default(),
                running: matches!(
                    session.state,
                    SessionState::Running | SessionState::AwaitingApproval
                ),
                turns: self.sessions.costs(&session.id).unwrap_or_default(),
            })
            .collect()
    }
}
