//! Running a round of tool calls: policy, the approval wait, execution and
//! the answer each call is owed (PLAN 4.2, PLAN 3).

use super::*;

use crate::store::parked::ParkCause;

impl Turn<'_> {
    /// Runs one round of tool calls in order, one at a time, because a person
    /// approves them one at a time. `async` for [`Turn::ask`] and `shell_exec`.
    pub(super) async fn execute(
        &self,
        plan: &TurnPlan,
        calls: &[AssembledCall],
        offered: Offered<'_>,
        cancel: &CancellationToken,
        progress_seq: &AtomicU32,
        skill: &mut Option<String>,
    ) {
        let Offered { held, connectors } = offered;
        for call in calls {
            if cancel.is_cancelled() {
                self.abandon(plan, call);
                continue;
            }

            // Cloned: the context borrows it while the run is updated after.
            let running = skill.clone();

            let args = match &call.args {
                Ok(args) => args.clone(),
                // Never executed, always answered (PLAN 4.1).
                Err(reason) => {
                    self.answer(
                        plan,
                        call,
                        ToolResult::refusal(&call.name, ErrorCode::ToolFailed, reason),
                        ToolCallStatus::Error,
                        reason.clone(),
                        None,
                    );
                    continue;
                }
            };

            self.sink.emit(Event::ToolRequested(ToolRequested {
                session_id: plan.session_id.clone(),
                turn_id: plan.turn_id.clone(),
                call_id: call.call_id.clone(),
                tool: call.name.clone(),
                args_redacted: crate::audit::redact(&args),
            }));

            let progress = Progress {
                sink: self.sink,
                session_id: &plan.session_id,
                turn_id: &plan.turn_id,
                call_id: &call.call_id,
                seq: progress_seq,
            };
            let ctx = ToolCtx {
                session_id: &plan.session_id,
                agent_id: &self.agent.id,
                turn_id: &plan.turn_id,
                call_id: &call.call_id,
                audit: self.audit,
                captures: self.captures,
                args: &args,
                progress: &progress,
                cancel,
                skills: SkillCtx {
                    library: self.skills,
                    workspace: plan.workspace.as_deref(),
                    tools: held,
                    active: running.as_deref(),
                },
                memories: self.memories,
                handoffs: self.standing.ctx(),
                connectors: self.connectors,
                routine: self.routine(),
                decision: self.decision,
            };

            // Only a capture needs the display geometry, passed in so policy
            // stays pure.
            let screen = (call.name == policy::tool::SCREEN_CAPTURE)
                .then(tools::screenshot::geometry)
                .flatten();
            // What an answer to a parked ask matches (PLAN 7.22): this tool
            // with exactly these arguments, digested the way the audit line
            // digests them.
            let fingerprint = crate::audit::fingerprint(&call.name, &args);
            let policy_ctx =
                PolicyCtx::new(&plan.session_id, plan.workspace.as_deref(), self.grants)
                    .with_fingerprint(Some(&fingerprint))
                    .with_self_exe(self.self_exe)
                    .with_exec_host(plan.exec_host.as_ref())
                    .with_screen(screen.as_ref())
                    .with_connectors(Some(connectors))
                    .with_decision_model(self.decision.map(DecisionClient::model))
                    .with_identity(Identity {
                        name: &self.agent.name,
                        tools: held,
                        skills: &self.agent.skills,
                    });
            let policy_ctx = if self.standing.open().is_some() {
                policy_ctx.delegated()
            } else {
                policy_ctx
            }
            .unattended(self.unattended.is_some());

            let judged = match policy::decide(&policy_ctx, &call.name, args.clone()) {
                Decision::Auto {
                    call: resolved,
                    reason,
                } => {
                    self.starting(plan, call);
                    Some(tools::run(&ctx, AuditDecision::Auto, reason, &resolved).await)
                }

                // Nobody could answer, so the question is kept rather than
                // thrown away (PLAN 7.22). Nothing runs either way.
                Decision::Park { request } => Some(self.park(
                    &ctx,
                    plan,
                    call,
                    &request,
                    &fingerprint,
                    ParkCause::Unattended,
                )),

                // A hard denial (PLAN 3.2). Never offered to the user, because
                // approving it could not mean anything.
                Decision::Deny { code, reason } => Some(tools::refuse(
                    &ctx,
                    &call.name,
                    AuditDecision::Deny,
                    code,
                    &reason,
                )),

                Decision::Ask {
                    call: resolved,
                    request,
                } => match self.ask(plan, call, &request, cancel).await {
                    // Cancelled while the dialog was open. Recorded as an
                    // abandoned call rather than a refusal: nobody said no.
                    None => {
                        self.abandon(plan, call);
                        continue;
                    }
                    Some(answer) if answer.decision.allows() => {
                        self.starting(plan, call);
                        Some(
                            tools::run(&ctx, answer.decision.audit(), &request.reason, &resolved)
                                .await,
                        )
                    }
                    // Five minutes with nobody at the screen used to throw the
                    // expensive part away (`IDEAS.md` § 5). It parks instead:
                    // the turn ends as it does on a denial, and the question
                    // keeps the arguments.
                    Some(answer) if answer.resolved_by == ResolvedBy::Timeout => Some(self.park(
                        &ctx,
                        plan,
                        call,
                        &request,
                        &fingerprint,
                        ParkCause::Expired,
                    )),
                    Some(answer) => Some(tools::refuse(
                        &ctx,
                        &call.name,
                        AuditDecision::Deny,
                        ErrorCode::Denied,
                        &refusal(answer),
                    )),
                },
            };

            let Some(outcome) = judged else {
                continue;
            };

            // Before the transcript is touched, so the next call of this same
            // round is already inside the run a `skill_run` just opened.
            skills::track(skill, &call.name, &outcome.result);

            // A scheduled run's answer, read from the envelope for the
            // scheduler (Phase 16).
            if let Some(unattended) = self.unattended {
                if let Some(returned) = skills::returned(&call.name, &outcome.result) {
                    unattended.reported.close(returned);
                }
            }

            // Keyed on the audit outcome, so a refusal reads as refused.
            let status = match outcome.audit.outcome {
                Outcome::Denied => ToolCallStatus::Denied,
                // A command killed by a Stop is not a tool that failed. The
                // transcript says so, and the model is told the same thing.
                Outcome::Cancelled => ToolCallStatus::Cancelled,
                _ if outcome.result.ok => ToolCallStatus::Ok,
                _ => ToolCallStatus::Error,
            };
            self.finish_call(plan, call, &outcome, status);
        }
    }

    /// Files one call for a person to answer later (PLAN 7.22), and gives the
    /// model the envelope that says so.
    ///
    /// Always a refusal on the wire: a parked call did not run. The code is
    /// `E_PARKED` rather than `E_DENIED`, because the question is still open.
    pub(super) fn park(
        &self,
        ctx: &ToolCtx<'_>,
        plan: &TurnPlan,
        call: &AssembledCall,
        request: &AskRequest,
        fingerprint: &str,
        cause: ParkCause,
    ) -> ToolOutcome {
        let refuse = |code: ErrorCode, reason: &str| {
            tools::refuse(ctx, &call.name, AuditDecision::Deny, code, reason)
        };

        let Some(parking) = self.parking else {
            return refuse(ErrorCode::Denied, &park::unsigned(request));
        };

        let parked = park::park(
            parking,
            park::Call {
                session_id: &plan.session_id,
                agent_id: &self.agent.id,
                turn_id: &plan.turn_id,
                call_id: &call.call_id,
                fingerprint,
                cause,
            },
            request,
            self.sink,
        );

        match parked {
            park::Outcome::Held(ask) => refuse(ErrorCode::Parked, &park::envelope(&ask)),
            park::Outcome::Refused(reason) => refuse(ErrorCode::Denied, &reason),
        }
    }

    /// Parks the turn until a person answers. `None` means the turn was
    /// cancelled, which is not a denial. On every exit the request is
    /// withdrawn, the session stops waiting, and `tool:approval_resolved` is
    /// emitted, so no dialog outlives its call.
    pub(super) async fn ask(
        &self,
        plan: &TurnPlan,
        call: &AssembledCall,
        request: &AskRequest,
        cancel: &CancellationToken,
    ) -> Option<Answer> {
        let ticket =
            self.approvals
                .register(&plan.session_id, &plan.turn_id, &call.call_id, request);
        let request_id = ticket.request.request_id.clone();

        self.turns
            .set_waiting(&plan.session_id, &plan.turn_id, true);
        self.session_changed(plan);
        self.sink
            .emit(Event::ToolApprovalRequired(Box::new(ticket.request)));

        // PLAN 7.18: the dialog is already open; an annotation that arrives
        // while it is still waiting is attached, one that does not is dropped.
        // It never answers the request.
        let answered = {
            let annotate = self.annotate(&plan.session_id, &request_id, request);
            let waiting = tokio::time::timeout(APPROVAL_TTL, ticket.answer);
            tokio::pin!(annotate, waiting);
            let mut annotating = self
                .decision
                .is_some_and(DecisionClient::annotates_approvals);

            // `biased`: a Stop that lands alongside an answer wins.
            let answered = loop {
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => break None,
                    answered = &mut waiting => break Some(answered),
                    () = &mut annotate, if annotating => annotating = false,
                }
            };
            if annotating {
                tracing::debug!(
                    request_id = %request_id,
                    "the approval was answered before its risk annotation arrived"
                );
            }
            answered
        };
        let answer = match answered {
            None => None,
            Some(answered) => match answered {
                Ok(Ok(answer)) => Some(answer),
                // The sender was dropped without an answer: the session was
                // deleted, or the registry was cleared under us.
                Ok(Err(_)) => Some(Answer {
                    decision: Answered::Deny,
                    resolved_by: ResolvedBy::Policy,
                }),
                Err(_elapsed) => {
                    tracing::info!(
                        session_id = %plan.session_id,
                        request_id = %request_id,
                        "an approval expired unanswered"
                    );
                    Some(Answer {
                        decision: Answered::Deny,
                        resolved_by: ResolvedBy::Timeout,
                    })
                }
            },
        };

        // Idempotent; covers the exits that were not an answer.
        self.approvals.withdraw(&request_id);
        self.turns
            .set_waiting(&plan.session_id, &plan.turn_id, false);

        let reported = answer.unwrap_or(Answer {
            decision: Answered::Deny,
            resolved_by: ResolvedBy::Policy,
        });
        self.sink
            .emit(Event::ToolApprovalResolved(ToolApprovalResolved {
                session_id: plan.session_id.clone(),
                turn_id: plan.turn_id.clone(),
                request_id,
                call_id: call.call_id.clone(),
                decision: reported.decision,
                resolved_by: reported.resolved_by,
            }));
        self.session_changed(plan);

        answer
    }

    /// Runs `tool_risk` for one open request and attaches what it found.
    pub(super) async fn annotate(&self, session_id: &str, request_id: &str, request: &AskRequest) {
        let Some(client) = self.decision.filter(|client| client.annotates_approvals()) else {
            return;
        };
        let Some(annotation) = tool_risk::annotate(client, request).await else {
            return;
        };
        let raised = annotation.raised;
        if self.approvals.annotate(request_id, annotation.clone()) {
            tracing::info!(request_id, raised, "an approval was annotated");
            self.sink.emit(Event::ToolApprovalAnnotated(Box::new(
                ToolApprovalAnnotated {
                    session_id: session_id.to_owned(),
                    request_id: request_id.to_owned(),
                    annotation,
                },
            )));
        }
    }

    /// Emits `tool:started` for a call that policy — or the user — cleared.
    pub(super) fn starting(&self, plan: &TurnPlan, call: &AssembledCall) {
        self.sink.emit(Event::ToolStarted(ToolStarted {
            session_id: plan.session_id.clone(),
            turn_id: plan.turn_id.clone(),
            call_id: call.call_id.clone(),
            tool: call.name.clone(),
        }));
    }

    /// Re-sends the session's row at the registry's state, so the sidebar and
    /// `session_open` agree.
    pub(super) fn session_changed(&self, plan: &TurnPlan) {
        let state = self.turns.state_of(&plan.session_id);
        if let Some(summary) = summarize(self.sessions, &plan.session_id, state) {
            self.sink.emit(Event::SessionUpdated(Box::new(summary)));
        }
    }

    /// Answers every call of a round without running it (a loop or the round
    /// ceiling). Every call needs a `tool` message, or the next request is
    /// invalid ([`transcript`]).
    pub(super) fn refuse_all(
        &self,
        plan: &TurnPlan,
        calls: &[AssembledCall],
        held: &[String],
        skill: Option<&str>,
        halt: Halt,
    ) {
        let refused = CancellationToken::new();
        let reason = halt.reason(skill);

        for call in calls {
            let ctx = ToolCtx {
                session_id: &plan.session_id,
                agent_id: &self.agent.id,
                turn_id: &plan.turn_id,
                call_id: &call.call_id,
                audit: self.audit,
                captures: self.captures,
                args: call.args.as_ref().unwrap_or(&serde_json::Value::Null),
                // Nothing runs down this path, so nothing produces output and
                // nothing is there to cancel.
                progress: &NullProgress,
                cancel: &refused,
                skills: SkillCtx {
                    library: self.skills,
                    workspace: plan.workspace.as_deref(),
                    tools: held,
                    // The halt was reached inside whatever run was open, and
                    // the refusals it produces belong to that run.
                    active: skill,
                },
                routine: self.routine(),
                memories: self.memories,
                handoffs: self.standing.ctx(),
                connectors: self.connectors,
                decision: self.decision,
            };
            let outcome =
                tools::refuse(&ctx, &call.name, AuditDecision::Deny, halt.code(), &reason);
            self.finish_call(plan, call, &outcome, ToolCallStatus::Denied);
        }
    }

    /// Records a call a cancel arrived before. Still answered, so later
    /// requests stay valid.
    pub(super) fn abandon(&self, plan: &TurnPlan, call: &AssembledCall) {
        let message = "the turn was cancelled before this call ran";
        self.answer(
            plan,
            call,
            ToolResult::refusal(&call.name, ErrorCode::Cancelled, message),
            ToolCallStatus::Cancelled,
            message.to_owned(),
            None,
        );
    }

    /// Emits `tool:finished`, updates the transcript and appends the answer.
    pub(super) fn finish_call(
        &self,
        plan: &TurnPlan,
        call: &AssembledCall,
        outcome: &ToolOutcome,
        status: ToolCallStatus,
    ) {
        self.sink.emit(Event::ToolFinished(ToolFinished {
            session_id: plan.session_id.clone(),
            turn_id: plan.turn_id.clone(),
            call_id: call.call_id.clone(),
            outcome: if outcome.result.ok {
                Outcome::Ok
            } else if status == ToolCallStatus::Denied {
                Outcome::Denied
            } else {
                Outcome::Error
            },
            summary: outcome.summary.clone(),
            duration_ms: outcome.audit.duration_ms,
            truncated: outcome.result.truncated,
            image_path: outcome.image_path.clone(),
        }));
        self.sink
            .emit(Event::AuditAppended(Box::new(outcome.audit.clone())));

        self.answer(
            plan,
            call,
            outcome.result.clone(),
            status,
            outcome.summary.clone(),
            outcome.image_path.clone(),
        );
    }

    /// Writes a call's result into the transcript: the record's status, and
    /// the `tool` message the next request will carry.
    pub(super) fn answer(
        &self,
        plan: &TurnPlan,
        call: &AssembledCall,
        result: ToolResult,
        status: ToolCallStatus,
        summary: String,
        image_path: Option<String>,
    ) {
        if let Err(err) = self.sessions.set_tool_call_status(
            &plan.session_id,
            &call.call_id,
            status,
            Some(summary),
            image_path,
        ) {
            tracing::warn!(%err, call_id = %call.call_id, "could not update the tool call record");
        }

        let message = Message::tool(&call.call_id, result.to_json());
        match self
            .sessions
            .append(&plan.session_id, message, SessionState::Running)
        {
            Ok(summary) => self.sink.emit(Event::SessionUpdated(Box::new(summary))),
            Err(err) => {
                tracing::warn!(%err, call_id = %call.call_id, "could not record the tool result");
            }
        }
    }
}
