//! The turn state machine (PLAN 4.2).
//!
//! ```text
//! session_send
//!   └─> Turn { id, session_id, cancel }
//!       Building -> Streaming -> [ToolPending -> Executing -> Building]* -> Done
//! ```
//!
//! One function owns the whole shape, because the transitions are the design:
//! a tool result must be persisted before the next request is built, the
//! assistant message must be on disk before its calls run, and every exit —
//! finished, cancelled, failed — must leave the session idle and the UI told.
//! Splitting that across modules is how a state machine grows a state nothing
//! resets.
//!
//! Three properties are worth stating because they are what the code is
//! arranged around, not incidental to it.
//!
//! **Cancellation is checked at every await.** The stream is consumed inside a
//! `select!` with the turn's [`CancellationToken`], so a cancel lands between
//! two tokens rather than after the reply completes. Text already streamed is
//! persisted rather than discarded: the user saw it, and a transcript that
//! disagrees with what was on screen is worse than a short one.
//!
//! **Deltas are coalesced into frames.** The WebView is woken about twenty
//! times a second instead of once per token (PLAN 4.2). A frame is opened by
//! the first token and closed by a timer, so a slow trickle still arrives
//! promptly rather than waiting for a token that never comes.
//!
//! **A denial is a result.** Policy refusing a call, a user refusing one, an
//! approval nobody answered, arguments that never parsed, the round cap — all
//! of them become an ordinary `tool` message with `ok: false`, and the turn
//! continues. The model reads it, explains itself and tries something else
//! (PLAN 4.3). The only things that end a turn early are cancellation and a
//! provider failure.
//!
//! **Waiting for a person is a state, not a stall.** When policy asks, the
//! turn registers the request, marks the session `awaiting_approval` and parks
//! on a `oneshot`. The wait is inside the same `select!` as the cancel token
//! and under a five-minute deadline, so neither a user who walks away nor one
//! who presses stop leaves a turn holding a call forever.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use tokio::sync::mpsc;
use tokio::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

use crate::approval::{Answer, ApprovalRegistry, Decision as Answered, ResolvedBy, APPROVAL_TTL};
use crate::audit::{AuditDecision, AuditLog, Outcome};
use crate::error::ErrorCode;
use crate::policy::{self, AskRequest, Decision, GrantStore, Identity, PolicyCtx};
use crate::skills::{self, SkillCtx};
use crate::store::{
    Agent, Message, SessionState, SessionStore, SessionSummary, ToolCallRecord, ToolCallStatus,
};
use crate::tools::{self, NullProgress, ProgressSink, Stream, ToolCtx, ToolOutcome, ToolResult};
use crate::workspace;

use super::event::{
    Event, EventSink, ToolApprovalResolved, ToolFinished, ToolProgress, ToolRequested, ToolStarted,
    TurnDelta, TurnError, TurnFinished, TurnMessage, TurnStarted,
};
use super::provider::Provider;
use super::registry::TurnRegistry;
use super::transcript;
use super::wire::{AssembledCall, ModelEvent, StopReason, Usage};

/// Tool rounds allowed in one turn (PLAN 4.2).
///
/// The round after this one is not executed: its calls are answered with
/// `E_TOO_MANY_TOOL_ROUNDS` and the turn finishes cleanly, which is a model
/// that can explain itself rather than a loop that runs until someone notices.
pub const MAX_TOOL_ROUNDS: u32 = 8;

/// How long a `turn:delta` frame stays open.
pub const DELTA_FRAME: Duration = Duration::from_millis(50);

/// What a turn needs to know about itself.
#[derive(Debug, Clone)]
pub struct TurnPlan {
    /// The session it belongs to.
    pub session_id: String,
    /// This turn, unique within the process.
    pub turn_id: String,
    /// The session's workspace root, canonical.
    ///
    /// `None` when the project's folder is gone. Every tool call is then a
    /// hard `E_NO_WORKSPACE` denial (PLAN 3.2), and the system message says so
    /// rather than letting the model find out one refusal at a time.
    pub workspace: Option<PathBuf>,
}

/// Everything the loop borrows for the length of one turn.
///
/// A struct of references rather than eight arguments: the call site builds it
/// once from [`AppState`](crate::state::AppState), and adding a dependency in
/// a later phase does not re-thread every signature.
pub struct Turn<'a> {
    /// The identity this turn runs as (PLAN 7.3, Phase 12).
    ///
    /// Resolved once per turn by the caller rather than read here, for the same
    /// reason the provider is: an identity can be edited between two messages
    /// in the same session, and a turn that re-read it mid-round could show the
    /// model one set of tools and then judge its calls against another.
    ///
    /// It reaches three places, and only three: the system message (who this
    /// is), the tool schemas (what it may ask for), and the policy context
    /// (what it may actually do). Nothing else in the loop branches on it.
    pub agent: &'a Agent,
    /// Where messages are read from and written to.
    pub sessions: &'a SessionStore,
    /// Which sessions are running, and how a blocked one is marked.
    ///
    /// The turn writes to this rather than only reading it: parking on an
    /// approval is a fact about the session that `session_open` and the
    /// sidebar both have to see, and the registry is where that fact already
    /// lives.
    pub turns: &'a TurnRegistry,
    /// Live `allow_session` grants.
    pub grants: &'a GrantStore,
    /// Where an approval waits for its answer.
    pub approvals: &'a ApprovalRegistry,
    /// Where tool calls are recorded.
    pub audit: &'a AuditLog,
    /// Who answers.
    pub provider: &'a dyn Provider,
    /// Where events go.
    pub sink: &'a dyn EventSink,
    /// This application's own binary, so `shell_exec` can refuse to run it.
    pub self_exe: Option<&'a Path>,
    /// Where `screen_capture` writes its PNGs.
    ///
    /// Aegis' own directory, never the workspace (PLAN 5.4): a capture is an
    /// artefact of the harness, and one landing in a project folder would end
    /// up in someone's next commit.
    pub captures: &'a Path,
    /// The user's skill library (PLAN 7.3, Phase 13).
    ///
    /// One of the two places a runbook is found; the other is the workspace,
    /// which the turn already knows. Passed in rather than derived for the
    /// reason `captures` is: where Aegis keeps its own files is a fact about
    /// the installation, and a loop that went looking for it could not be run
    /// in a test without one.
    pub skills: &'a Path,
}

/// The [`ProgressSink`] one tool call writes its live output to.
///
/// A tool produces text; the turn decides what that text *is* on the wire —
/// which session and call it belongs to, and where it falls in the turn's
/// sequence. Keeping the numbering here rather than in the tool is what lets
/// the UI drop a duplicated or reordered frame with one rule, and what keeps
/// `tools/` from having to know anything about events.
struct Progress<'a> {
    /// Where the event goes.
    sink: &'a dyn EventSink,
    /// The session.
    session_id: &'a str,
    /// The turn.
    turn_id: &'a str,
    /// The call whose output this is.
    call_id: &'a str,
    /// The turn's frame counter, shared by every call in it.
    seq: &'a AtomicU32,
}

impl ProgressSink for Progress<'_> {
    fn chunk(&self, stream: Stream, text: &str) {
        // `Relaxed` is enough: the ordering that matters is the `seq` value
        // itself, which the UI reads, and no other state is published with it.
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);

        self.sink.emit(Event::ToolProgress(ToolProgress {
            session_id: self.session_id.to_owned(),
            turn_id: self.turn_id.to_owned(),
            call_id: self.call_id.to_owned(),
            stream,
            seq,
            chunk: text.to_owned(),
        }));
    }
}

/// How one round of streaming ended.
enum Streamed {
    /// The provider finished normally.
    Completed {
        text: String,
        calls: Vec<AssembledCall>,
        reason: StopReason,
        usage: Option<Usage>,
    },
    /// The user cancelled mid-stream.
    Cancelled { text: String },
    /// The provider itself failed.
    Failed {
        text: String,
        code: String,
        message: String,
        retryable: bool,
    },
}

impl Turn<'_> {
    /// Runs a turn to completion and reports how it ended.
    ///
    /// Never returns an `Err`. Everything that can go wrong is either a tool
    /// result the model reads or a `turn:error` the user reads, and a turn
    /// that returned a `Result` would make the caller decide which — a
    /// decision it has less information to make than this function does.
    pub async fn run(&self, plan: &TurnPlan, cancel: &CancellationToken) -> StopReason {
        self.sink.emit(Event::TurnStarted(TurnStarted {
            session_id: plan.session_id.clone(),
            turn_id: plan.turn_id.clone(),
            model: self.provider.model().to_owned(),
        }));

        let mut seq = 0u32;
        let mut rounds = 0u32;
        let mut usage = None;

        // Read once per turn rather than per round. A runbook can be edited
        // between two messages and the next turn picks that up, but a catalog
        // that changed halfway through a turn would show the model one list
        // and then judge its choice against another — the same reason the
        // identity and the provider are resolved once, above this call.
        //
        // The catalog is a line per runbook. The bodies stay on disk until
        // `skill_run` asks for one (PLAN 7.6).
        let catalog = skills::catalog(self.skills, plan.workspace.as_deref());
        let offered = skills::granted(&catalog, self.agent);
        let skill_block = skills::prompt_block(&offered);

        // Which runbook this turn is currently following, if any. A local, so
        // it cannot outlive the turn: the body was loaded into this turn and
        // the span it names is this turn's (see `skills::track`).
        let mut skill: Option<String> = None;

        // Per-turn, like the delta counter, and shared with every tool call in
        // the turn — the UI drops anything out of order, and a counter that
        // restarted per call would make two calls' frames indistinguishable
        // after a reload. Atomic because the sink that bumps it is handed to a
        // tool as a `&dyn`, and a tool has no business holding a `&mut` to the
        // turn's state.
        let progress_seq = AtomicU32::new(0);

        let reason = loop {
            let history = match self.sessions.messages(&plan.session_id) {
                Ok(history) => history,
                Err(err) => {
                    // The session was deleted while its turn was running.
                    tracing::warn!(%err, session_id = %plan.session_id, "the turn lost its session");
                    break self.fail(plan, ErrorCode::Internal, &err.to_string(), false);
                }
            };

            // Read fresh for every round, not once per turn: this *is* the
            // read path of the workspace convention (PLAN 7.3, Phase 11), and
            // a round that has just written `DECISIONS.md` should see it in
            // the next one rather than argue with a stale copy of itself.
            // `None` for a workspace that does not use the convention, which
            // leaves the prompt exactly as it was before that phase.
            let shared = plan.workspace.as_deref().and_then(workspace::digest);

            let request = transcript::build(
                self.provider.model(),
                self.agent,
                &history,
                plan.workspace.as_deref(),
                skill_block.as_deref(),
                shared.as_deref(),
                // Half of the tool ACL, and the half the model can see: an
                // identity that was not granted `shell_exec` is not offered
                // one, so it never spends a round asking for it. The other half
                // is the refusal in `policy::decide_call`, which is what catches
                // a call replayed out of a transcript written under a wider
                // grant.
                tools::schemas_for(&self.agent.tools),
            );

            let stream = self.provider.stream(request);
            match self.consume(plan, stream, cancel, &mut seq).await {
                Streamed::Cancelled { text } => {
                    self.persist_assistant(plan, text, Vec::new());
                    break StopReason::Cancelled;
                }
                Streamed::Failed {
                    text,
                    code,
                    message,
                    retryable,
                } => {
                    self.persist_assistant(plan, text, Vec::new());
                    self.sink.emit(Event::TurnError(TurnError {
                        session_id: plan.session_id.clone(),
                        turn_id: plan.turn_id.clone(),
                        code,
                        message,
                        retryable,
                    }));
                    break StopReason::Error;
                }
                Streamed::Completed {
                    text,
                    calls,
                    reason,
                    usage: reported,
                } => {
                    usage = reported.or(usage);

                    let records: Vec<ToolCallRecord> = calls.iter().map(record_of).collect();
                    self.persist_assistant(plan, text, records);

                    if calls.is_empty() {
                        break reason;
                    }

                    // The round after the cap is answered, not executed. The
                    // model sees why it stopped and the turn ends cleanly.
                    if rounds >= MAX_TOOL_ROUNDS {
                        tracing::warn!(
                            session_id = %plan.session_id,
                            rounds,
                            "tool round cap reached"
                        );
                        self.refuse_all(plan, &calls, skill.as_deref());
                        break StopReason::Stop;
                    }

                    self.execute(plan, &calls, cancel, &progress_seq, &mut skill)
                        .await;
                    rounds += 1;

                    if cancel.is_cancelled() {
                        break StopReason::Cancelled;
                    }
                }
            }
        };

        // A run that never returned. Said out loud rather than carried into
        // the next turn: the runbook was loaded into *this* one, and a span
        // that outlived it would put a skill's name on calls made after the
        // user had moved the conversation somewhere else.
        if let Some(unfinished) = &skill {
            tracing::warn!(
                session_id = %plan.session_id,
                turn_id = %plan.turn_id,
                skill = %unfinished,
                "the turn ended without a skill_return"
            );
        }

        self.sink.emit(Event::TurnFinished(TurnFinished {
            session_id: plan.session_id.clone(),
            turn_id: plan.turn_id.clone(),
            stop_reason: reason,
            usage,
        }));

        tracing::info!(
            session_id = %plan.session_id,
            turn_id = %plan.turn_id,
            ?reason,
            rounds,
            "turn finished"
        );
        reason
    }

    // -----------------------------------------------------------------------
    // Streaming
    // -----------------------------------------------------------------------

    /// Consumes one response, coalescing text and assembling tool calls.
    ///
    /// The `select!` is `biased` so cancellation is polled before new events:
    /// a turn that is being cancelled should not first drain whatever the
    /// provider has already buffered.
    async fn consume(
        &self,
        plan: &TurnPlan,
        mut stream: mpsc::Receiver<ModelEvent>,
        cancel: &CancellationToken,
        seq: &mut u32,
    ) -> Streamed {
        let mut text = String::new();
        let mut frame = String::new();
        let mut assembler = super::wire::ToolCallAssembler::default();
        let mut deadline: Option<Instant> = None;

        let mut reason = None;
        let mut failure = None;

        loop {
            // With no open frame there is nothing to flush, so the timer must
            // never fire; `pending` is the future that never completes.
            let tick = async {
                match deadline {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending::<()>().await,
                }
            };

            tokio::select! {
                biased;

                () = cancel.cancelled() => {
                    self.flush(plan, &mut frame, seq);
                    tracing::debug!(turn_id = %plan.turn_id, "cancelled mid-stream");
                    return Streamed::Cancelled { text };
                }

                () = tick => {
                    self.flush(plan, &mut frame, seq);
                    deadline = None;
                }

                event = stream.recv() => {
                    let Some(event) = event else { break };

                    match event {
                        ModelEvent::TextDelta { text: delta } => {
                            text.push_str(&delta);
                            frame.push_str(&delta);
                            // The first token of a frame opens the window; the
                            // rest ride along inside it.
                            if deadline.is_none() {
                                deadline = Some(Instant::now() + DELTA_FRAME);
                            }
                        }
                        ModelEvent::ToolCallDelta { index, id, name, args_delta } => {
                            assembler.push(index, id, name, &args_delta);
                        }
                        ModelEvent::Finish { reason: stop, usage } => {
                            reason = Some((stop, usage));
                            break;
                        }
                        ModelEvent::Error { code, message, retryable } => {
                            failure = Some((code, message, retryable));
                            break;
                        }
                    }
                }
            }
        }

        self.flush(plan, &mut frame, seq);

        if let Some((code, message, retryable)) = failure {
            return Streamed::Failed {
                text,
                code,
                message,
                retryable,
            };
        }

        let Some((reason, usage)) = reason else {
            // The channel closed without a `Finish`. The reply is truncated
            // and there is no honest stop reason to report, so it is a
            // failure rather than a short success.
            return Streamed::Failed {
                text,
                code: ErrorCode::ProviderParse.as_str().to_owned(),
                message: "the provider closed the stream without finishing the reply".to_owned(),
                retryable: true,
            };
        };

        let calls = assembler.finish();
        let reason = if calls.is_empty() {
            reason
        } else {
            // Some servers report `stop` even when they streamed tool calls.
            // What arrived decides, not what was claimed.
            StopReason::ToolCalls
        };

        Streamed::Completed {
            text,
            calls,
            reason,
            usage,
        }
    }

    /// Emits whatever text has accumulated, and empties the frame.
    fn flush(&self, plan: &TurnPlan, frame: &mut String, seq: &mut u32) {
        if frame.is_empty() {
            return;
        }

        self.sink.emit(Event::TurnDelta(TurnDelta {
            session_id: plan.session_id.clone(),
            turn_id: plan.turn_id.clone(),
            seq: *seq,
            text: std::mem::take(frame),
        }));
        *seq = seq.saturating_add(1);
    }

    // -----------------------------------------------------------------------
    // Tools
    // -----------------------------------------------------------------------

    /// Runs one round of tool calls, in the order the model made them.
    ///
    /// Sequential rather than concurrent, and now for a reason stronger than
    /// filesystem races: the user approves these one at a time. Two dialogs
    /// competing for the same person's attention is not a queue, and a second
    /// call that ran while the first was still being read would have been
    /// approved by nobody.
    ///
    /// The filesystem tools themselves are short and local, so they still run
    /// inline rather than on a blocking thread. What makes this `async` is the
    /// waiting: [`Turn::ask`] parks here until a person answers, and
    /// `shell_exec` awaits a child process for as long as two minutes.
    async fn execute(
        &self,
        plan: &TurnPlan,
        calls: &[AssembledCall],
        cancel: &CancellationToken,
        progress_seq: &AtomicU32,
        skill: &mut Option<String>,
    ) {
        for call in calls {
            if cancel.is_cancelled() {
                self.abandon(plan, call);
                continue;
            }

            // Cloned rather than borrowed for the length of the call: the
            // context below holds it, and the run is updated from the result
            // once the call is done. One `String` per tool call is not worth a
            // lifetime that would have to be threaded through `ToolCtx`.
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
                    tools: &self.agent.tools,
                    active: running.as_deref(),
                },
            };

            // Measured for the one tool whose prompt names a display, and for
            // no other: asking the window server to describe the screen is a
            // round trip, and `fs_read` has no use for the answer. Policy
            // takes it as an argument rather than measuring it itself, which
            // is what keeps the decision table a pure function and testable
            // without a screen.
            let screen = (call.name == policy::tool::SCREEN_CAPTURE)
                .then(tools::screenshot::geometry)
                .flatten();
            let policy_ctx =
                PolicyCtx::new(&plan.session_id, plan.workspace.as_deref(), self.grants)
                    .with_self_exe(self.self_exe)
                    .with_screen(screen.as_ref())
                    .with_identity(Identity {
                        name: &self.agent.name,
                        tools: &self.agent.tools,
                        skills: &self.agent.skills,
                    });

            let judged = match policy::decide(&policy_ctx, &call.name, args.clone()) {
                Decision::Auto {
                    call: resolved,
                    reason,
                } => {
                    self.starting(plan, call);
                    Some(tools::run(&ctx, AuditDecision::Auto, reason, &resolved).await)
                }

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

            // Keyed on what was audited rather than on `ok` alone, so a call
            // that was refused reads as refused in the transcript instead of
            // as one that ran and failed.
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

    /// Parks the turn until a person answers, and reports what they said.
    ///
    /// `None` means the turn was cancelled while the dialog was open. That is
    /// deliberately not a denial: "you said no" and "you stopped the turn" are
    /// different things to write into a transcript, and only one of them is a
    /// decision about the call.
    ///
    /// Three things are true on every exit from this function, however it
    /// exits: the request is no longer answerable, the session is no longer
    /// marked as waiting, and `tool:approval_resolved` has been emitted. A
    /// dialog left on screen for a call nothing will ever run is the failure
    /// this shape exists to prevent.
    async fn ask(
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

        // `biased` so a cancel that arrives alongside an answer wins: the user
        // pressed stop, and a call that ran anyway would be one they stopped.
        let answer = tokio::select! {
            biased;
            () = cancel.cancelled() => None,
            answered = tokio::time::timeout(APPROVAL_TTL, ticket.answer) => match answered {
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

        // Idempotent: an answered request was already removed by `resolve`.
        // This covers the other exits, and closes the window in which a click
        // could land on a request nothing is waiting for.
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

    /// Emits `tool:started` for a call that policy — or the user — cleared.
    fn starting(&self, plan: &TurnPlan, call: &AssembledCall) {
        self.sink.emit(Event::ToolStarted(ToolStarted {
            session_id: plan.session_id.clone(),
            turn_id: plan.turn_id.clone(),
            call_id: call.call_id.clone(),
            tool: call.name.clone(),
        }));
    }

    /// Re-sends the session's row at whatever state the registry now reports.
    ///
    /// Read back from the registry rather than passed in, so the badge in the
    /// sidebar and the `state` a `session_open` returns cannot disagree about
    /// whether a session is working or waiting for the person looking at it.
    fn session_changed(&self, plan: &TurnPlan) {
        let state = self.turns.state_of(&plan.session_id);
        if let Some(summary) = summarize(self.sessions, &plan.session_id, state) {
            self.sink.emit(Event::SessionUpdated(summary));
        }
    }

    /// Answers every call of a round without running any of them.
    ///
    /// Used for the round cap: the model has to see one `tool` message per
    /// call it made, or the next request it appears in is structurally
    /// invalid (see [`transcript`]).
    fn refuse_all(&self, plan: &TurnPlan, calls: &[AssembledCall], skill: Option<&str>) {
        let refused = CancellationToken::new();

        for call in calls {
            let reason = format!(
                "this turn already ran {MAX_TOOL_ROUNDS} rounds of tools, which is the limit; \
                 answer with what you have, or ask the user to continue"
            );
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
                    tools: &self.agent.tools,
                    // The cap was reached inside whatever run was open, and
                    // the refusals it produces belong to that run.
                    active: skill,
                },
            };
            let outcome = tools::refuse(
                &ctx,
                &call.name,
                AuditDecision::Deny,
                ErrorCode::TooManyToolRounds,
                &reason,
            );
            self.finish_call(plan, call, &outcome, ToolCallStatus::Denied);
        }
    }

    /// Records a call that a cancel arrived before.
    ///
    /// Still answered, for the same structural reason: an unanswered call
    /// would make every later request in this session invalid.
    fn abandon(&self, plan: &TurnPlan, call: &AssembledCall) {
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
    fn finish_call(
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
    fn answer(
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
            Ok(summary) => self.sink.emit(Event::SessionUpdated(summary)),
            Err(err) => {
                tracing::warn!(%err, call_id = %call.call_id, "could not record the tool result");
            }
        }
    }

    // -----------------------------------------------------------------------
    // Persistence
    // -----------------------------------------------------------------------

    /// Persists the assistant message this round produced, if it produced one.
    ///
    /// An empty message is skipped: a round that produced no text and made no
    /// calls has nothing to say, and an empty bubble in the transcript reads
    /// as a bug.
    fn persist_assistant(&self, plan: &TurnPlan, text: String, calls: Vec<ToolCallRecord>) {
        let message = Message::assistant(text, calls);
        if message.is_empty() {
            return;
        }

        match self
            .sessions
            .append(&plan.session_id, message.clone(), SessionState::Running)
        {
            Ok(summary) => {
                self.sink.emit(Event::TurnMessage(Box::new(TurnMessage {
                    session_id: plan.session_id.clone(),
                    turn_id: plan.turn_id.clone(),
                    message,
                })));
                self.sink.emit(Event::SessionUpdated(summary));
            }
            Err(err) => {
                tracing::error!(%err, session_id = %plan.session_id, "could not persist the reply");
            }
        }
    }

    /// Emits a `turn:error` and reports the stop reason that follows it.
    fn fail(&self, plan: &TurnPlan, code: ErrorCode, message: &str, retryable: bool) -> StopReason {
        self.sink.emit(Event::TurnError(TurnError {
            session_id: plan.session_id.clone(),
            turn_id: plan.turn_id.clone(),
            code: code.as_str().to_owned(),
            message: message.to_owned(),
            retryable,
        }));
        StopReason::Error
    }
}

/// What the model is told about a call that was not allowed to run.
///
/// Written for the model rather than for a log: it says what happened, and
/// what to do next. A refusal the model reads as a transport failure is a
/// refusal it retries.
fn refusal(answer: Answer) -> String {
    match answer.resolved_by {
        ResolvedBy::User => "the user refused this call. Do not repeat it. Say what you were \
                             trying to do and let them decide, or carry on with what you can do \
                             without it"
            .to_owned(),
        ResolvedBy::Timeout => "nobody answered the approval for this call, so it was refused \
                                after five minutes. The user is probably away from the machine"
            .to_owned(),
        ResolvedBy::Policy => {
            "the approval for this call was withdrawn before anyone answered it".to_owned()
        }
    }
}

/// The transcript record for a call the model just made.
///
/// Status starts at `Pending`: policy has not seen it yet, and the UI draws
/// the card before the decision is known.
fn record_of(call: &AssembledCall) -> ToolCallRecord {
    ToolCallRecord {
        call_id: call.call_id.clone(),
        tool: call.name.clone(),
        args_json: call.args_json.clone(),
        status: ToolCallStatus::Pending,
        summary: None,
        // Filled in when the call finishes, and only by `screen_capture`.
        image_path: None,
    }
}

/// A [`SessionSummary`] for a session that is no longer running.
///
/// Exposed for the command layer, which has to leave the session in a state
/// the sidebar can draw once the turn's task is gone.
pub fn resting_state(reason: StopReason) -> SessionState {
    match reason {
        StopReason::Error => SessionState::Error,
        StopReason::Stop | StopReason::ToolCalls | StopReason::Cancelled | StopReason::Length => {
            SessionState::Idle
        }
    }
}

/// Convenience for a caller that needs the summary and the state together.
pub fn summarize(
    sessions: &SessionStore,
    session_id: &str,
    state: SessionState,
) -> Option<SessionSummary> {
    match sessions.summary(session_id, state) {
        Ok(summary) => Some(summary),
        Err(err) => {
            tracing::debug!(%err, session_id, "no summary for a session that is gone");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    use serde_json::json;
    use tempfile::TempDir;

    use crate::agent::provider::FakeProvider;
    use crate::agent::wire::ModelEvent;
    use crate::approval::{ApprovalRequest, Decision as Answered};
    use crate::policy::tool;
    use crate::policy::Grant;

    /// The turn every fixture registers, so `plan()` and the registry agree.
    const TURN_ID: &str = "t1";

    /// An [`EventSink`] that keeps everything, for assertions.
    #[derive(Debug, Default)]
    struct Recorder {
        events: Mutex<Vec<Event>>,
    }

    impl Recorder {
        fn events(&self) -> Vec<Event> {
            self.events
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }

        fn names(&self) -> Vec<&'static str> {
            self.events().iter().map(Event::name).collect()
        }

        /// The concatenated text of every `turn:delta`.
        fn streamed(&self) -> String {
            self.events()
                .iter()
                .filter_map(|event| match event {
                    Event::TurnDelta(delta) => Some(delta.text.clone()),
                    _ => None,
                })
                .collect()
        }

        fn finished(&self) -> Option<StopReason> {
            self.events().iter().find_map(|event| match event {
                Event::TurnFinished(finished) => Some(finished.stop_reason),
                _ => None,
            })
        }
    }

    impl EventSink for Recorder {
        fn emit(&self, event: Event) {
            self.events
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(event);
        }
    }

    /// A workspace, the stores over it, and a session ready to send into.
    struct Fixture {
        _dir: TempDir,
        workspace: PathBuf,
        sessions: SessionStore,
        turns: TurnRegistry,
        grants: GrantStore,
        approvals: ApprovalRegistry,
        audit: AuditLog,
        sink: Recorder,
        session_id: String,
        captures: PathBuf,
        /// An empty skill library. These tests are about the loop; the runner
        /// has its own, in `skills` and in `tests/skills.rs`.
        library: PathBuf,
        /// The identity every fixture turn runs as: the built-in one, which
        /// holds every tool, so these tests are about the loop and not about
        /// an allow-list.
        agent: Agent,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = TempDir::new().expect("temp dir");
            let data = dir.path().join("data");
            let workspace = dir.path().join("work");
            std::fs::create_dir_all(&data).expect("data dir");
            std::fs::create_dir_all(&workspace).expect("workspace dir");

            let captures = data.join("captures");
            let sessions = SessionStore::load(&data);
            let session_id = sessions
                .create("p1", None, crate::store::DEFAULT_AGENT_ID)
                .expect("session")
                .id;

            // Registered the way `session_send` registers it, because the
            // session's state — running, or waiting for a person — is read
            // back out of this registry and asserted on.
            let turns = TurnRegistry::new();
            turns.begin(&session_id, TURN_ID).expect("a free session");

            Self {
                workspace: dunce::canonicalize(&workspace).expect("canonical workspace"),
                _dir: dir,
                sessions,
                turns,
                grants: GrantStore::new(),
                approvals: ApprovalRegistry::new(),
                audit: AuditLog::new(&data),
                sink: Recorder::default(),
                session_id,
                captures,
                library: data.join("skills"),
                agent: Agent::builtin(),
            }
        }

        fn plan(&self) -> TurnPlan {
            TurnPlan {
                session_id: self.session_id.clone(),
                turn_id: TURN_ID.to_owned(),
                workspace: Some(self.workspace.clone()),
            }
        }

        fn turn<'a>(&'a self, provider: &'a dyn Provider) -> Turn<'a> {
            Turn {
                agent: &self.agent,
                sessions: &self.sessions,
                turns: &self.turns,
                grants: &self.grants,
                approvals: &self.approvals,
                audit: &self.audit,
                provider,
                sink: &self.sink,
                self_exe: None,
                captures: &self.captures,
                skills: &self.library,
            }
        }

        /// The approval the turn is currently blocked on.
        ///
        /// Polled rather than awaited on a channel: the turn is a task in the
        /// same runtime, and this is the shape a test uses to answer a dialog
        /// the way a click would.
        async fn pending(&self) -> ApprovalRequest {
            for _ in 0..200 {
                if let Some(request) = self
                    .approvals
                    .list(Some(&self.session_id))
                    .into_iter()
                    .next()
                {
                    return request;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            panic!("no approval was raised");
        }

        /// Answers whatever the turn is waiting on, the way the command does.
        async fn answer(&self, decision: Answered) {
            let request = self.pending().await;
            self.approvals
                .resolve(&request.request_id, decision, &self.grants)
                .expect("the request is open");
        }

        fn say(&self, text: &str) {
            self.sessions
                .append(&self.session_id, Message::user(text), SessionState::Running)
                .expect("append");
        }

        fn transcript(&self) -> Vec<Message> {
            self.sessions.messages(&self.session_id).expect("messages")
        }
    }

    /// Streams a tool call, then whatever the model says afterwards.
    fn tool_call_script(id: &str, name: &str, args: &str) -> Vec<ModelEvent> {
        vec![
            ModelEvent::ToolCallDelta {
                index: 0,
                id: Some(id.to_owned()),
                name: Some(name.to_owned()),
                args_delta: args.to_owned(),
            },
            ModelEvent::Finish {
                reason: StopReason::ToolCalls,
                usage: None,
            },
        ]
    }

    #[tokio::test]
    async fn a_plain_turn_streams_persists_and_finishes() {
        let fx = Fixture::new();
        fx.say("hello");

        let provider = FakeProvider::instant();
        let reason = fx
            .turn(&provider)
            .run(&fx.plan(), &CancellationToken::new())
            .await;

        assert_eq!(reason, StopReason::Stop);
        assert_eq!(fx.sink.finished(), Some(StopReason::Stop));

        let names = fx.sink.names();
        assert_eq!(names.first(), Some(&"turn:started"));
        assert_eq!(names.last(), Some(&"turn:finished"));
        assert!(names.contains(&"turn:message"));
        assert!(names.contains(&"session:updated"));

        // What was streamed is what was stored: the UI swaps its buffer for
        // the finalized message, and the two must agree.
        let transcript = fx.transcript();
        let reply = transcript.last().expect("a reply");
        assert_eq!(reply.role, crate::store::Role::Assistant);
        assert_eq!(fx.sink.streamed(), reply.text);
        assert!(reply.text.contains("hello"), "{}", reply.text);
    }

    /// Deltas are frames, not tokens: the WebView is woken about twenty times
    /// a second however fast the provider goes.
    #[tokio::test]
    async fn deltas_are_coalesced_into_frames() {
        let fx = Fixture::new();
        fx.say("hello");

        let provider = FakeProvider::instant();
        fx.turn(&provider)
            .run(&fx.plan(), &CancellationToken::new())
            .await;

        let deltas: Vec<u32> = fx
            .sink
            .events()
            .iter()
            .filter_map(|event| match event {
                Event::TurnDelta(delta) => Some(delta.seq),
                _ => None,
            })
            .collect();

        assert!(!deltas.is_empty(), "something streamed");
        let tokens = fx
            .transcript()
            .last()
            .expect("reply")
            .text
            .split(' ')
            .count();
        assert!(
            deltas.len() < tokens,
            "{} frames for {tokens} tokens is not coalescing",
            deltas.len()
        );

        // `seq` is monotonic from zero, which is what lets the UI drop
        // duplicates after a reload.
        let expected: Vec<u32> = (0..u32::try_from(deltas.len()).expect("small")).collect();
        assert_eq!(deltas, expected);
    }

    #[tokio::test]
    async fn cancelling_stops_the_turn_and_keeps_what_was_said() {
        let fx = Fixture::new();
        fx.say("hello");

        // Pacing is on, so the cancel lands mid-stream rather than after.
        let provider = FakeProvider::new();
        let cancel = CancellationToken::new();

        let cancel_after = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(60)).await;
            cancel_after.cancel();
        });

        let reason = fx.turn(&provider).run(&fx.plan(), &cancel).await;

        assert_eq!(reason, StopReason::Cancelled);
        assert_eq!(fx.sink.finished(), Some(StopReason::Cancelled));

        let transcript = fx.transcript();
        let reply = transcript.last().expect("a partial reply");
        assert_eq!(reply.role, crate::store::Role::Assistant);
        assert!(!reply.text.is_empty(), "what the user saw is kept");
        assert_eq!(
            fx.sink.streamed(),
            reply.text,
            "the stored partial is exactly what was streamed"
        );
    }

    #[tokio::test]
    async fn a_cancel_before_the_first_token_ends_the_turn_immediately() {
        let fx = Fixture::new();
        fx.say("hello");

        let cancel = CancellationToken::new();
        cancel.cancel();

        let provider = FakeProvider::new();
        let reason = fx.turn(&provider).run(&fx.plan(), &cancel).await;

        assert_eq!(reason, StopReason::Cancelled);
        assert!(
            !fx.sink.names().contains(&"turn:message"),
            "nothing was said, so nothing is stored"
        );
    }

    #[tokio::test]
    async fn a_tool_call_runs_under_policy_and_is_answered() {
        let fx = Fixture::new();
        std::fs::write(fx.workspace.join("a.txt"), "hello file").expect("write");
        fx.say("read a.txt");

        let provider = FakeProvider::scripted(vec![tool_call_script(
            "call_1",
            tool::FS_READ,
            r#"{"path":"a.txt"}"#,
        )]);

        let reason = fx
            .turn(&provider)
            .run(&fx.plan(), &CancellationToken::new())
            .await;
        assert_eq!(
            reason,
            StopReason::Stop,
            "the round after the tool finished"
        );

        let names = fx.sink.names();
        assert!(names.contains(&"tool:requested"), "{names:?}");
        assert!(names.contains(&"tool:started"), "{names:?}");
        assert!(names.contains(&"tool:finished"), "{names:?}");
        assert!(names.contains(&"audit:appended"), "{names:?}");

        let transcript = fx.transcript();
        let call = &transcript[1].tool_calls[0];
        assert_eq!(call.status, ToolCallStatus::Ok);
        assert!(call.summary.is_some());

        let answer = &transcript[2];
        assert_eq!(answer.role, crate::store::Role::Tool);
        assert_eq!(answer.tool_call_id.as_deref(), Some("call_1"));
        let envelope: serde_json::Value = serde_json::from_str(&answer.text).expect("an envelope");
        assert_eq!(envelope["ok"], json!(true));
        assert_eq!(envelope["content"], "hello file");

        // The audit log holds the call, whatever the UI did with the event.
        assert_eq!(fx.audit.tail(10, None).expect("tail").len(), 1);
    }

    /// A write inside the workspace is asked about. Allowing it once runs it,
    /// and leaves nothing behind that would skip the next prompt.
    #[tokio::test]
    async fn an_allowed_call_runs_and_grants_nothing() {
        let fx = Fixture::new();
        fx.say("write a file");

        let provider = FakeProvider::scripted(vec![tool_call_script(
            "call_1",
            tool::FS_WRITE,
            r#"{"path":"new.txt","content":"x"}"#,
        )]);

        let answering = async {
            fx.answer(Answered::AllowOnce).await;
        };
        let turn = fx.turn(&provider);
        let plan = fx.plan();
        let cancel = CancellationToken::new();
        let (reason, ()) = tokio::join!(turn.run(&plan, &cancel), answering);

        assert_eq!(reason, StopReason::Stop);
        assert_eq!(
            std::fs::read_to_string(fx.workspace.join("new.txt")).expect("the file"),
            "x"
        );

        let names = fx.sink.names();
        assert!(names.contains(&"tool:approval_required"), "{names:?}");
        assert!(names.contains(&"tool:approval_resolved"), "{names:?}");
        assert!(names.contains(&"tool:started"), "{names:?}");

        let transcript = fx.transcript();
        assert_eq!(transcript[1].tool_calls[0].status, ToolCallStatus::Ok);

        let audit = fx.audit.tail(10, None).expect("tail");
        assert_eq!(audit[0].decision, AuditDecision::AllowOnce);
        assert_eq!(audit[0].outcome, Outcome::Ok);

        assert!(
            fx.grants.list(&fx.session_id).is_empty(),
            "allow-once must not quietly become allow-session"
        );
        assert!(fx.approvals.is_empty(), "the request was consumed");
    }

    /// PLAN 6, Phase 6 exit: a denial lands in the transcript as `E_DENIED`
    /// without aborting the turn.
    #[tokio::test]
    async fn a_denial_is_a_result_and_the_turn_carries_on() {
        let fx = Fixture::new();
        fx.say("write a file");

        let provider = FakeProvider::scripted(vec![tool_call_script(
            "call_1",
            tool::FS_WRITE,
            r#"{"path":"new.txt","content":"x"}"#,
        )]);

        let answering = async {
            fx.answer(Answered::Deny).await;
        };
        let turn = fx.turn(&provider);
        let plan = fx.plan();
        let cancel = CancellationToken::new();
        let (reason, ()) = tokio::join!(turn.run(&plan, &cancel), answering);

        assert_eq!(reason, StopReason::Stop, "a denial does not end the turn");
        assert!(
            !fx.workspace.join("new.txt").exists(),
            "a denied write must not touch the disk"
        );
        assert!(
            !fx.sink.names().contains(&"tool:started"),
            "nothing was started"
        );

        let transcript = fx.transcript();
        assert_eq!(transcript[1].tool_calls[0].status, ToolCallStatus::Denied);

        let envelope: serde_json::Value =
            serde_json::from_str(&transcript[2].text).expect("an envelope");
        assert_eq!(envelope["ok"], json!(false));
        assert_eq!(envelope["error"]["code"], "E_DENIED");

        // The model is told it was the user, and told not to repeat it.
        let message = envelope["error"]["message"]
            .as_str()
            .expect("a message")
            .to_owned();
        assert!(message.contains("user refused"), "{message}");

        let audit = fx.audit.tail(10, None).expect("tail");
        assert_eq!(audit[0].decision, AuditDecision::Deny);
        assert_eq!(audit[0].outcome, Outcome::Denied);
    }

    /// The second half of `allow_session`: the grant is recorded, and the call
    /// behind it is not asked about again.
    #[tokio::test]
    async fn allowing_for_the_session_stops_the_next_prompt() {
        let fx = Fixture::new();
        fx.say("write two files");

        let provider = FakeProvider::scripted(vec![
            tool_call_script(
                "call_1",
                tool::FS_WRITE,
                r#"{"path":"one.txt","content":"1"}"#,
            ),
            tool_call_script(
                "call_2",
                tool::FS_WRITE,
                r#"{"path":"two.txt","content":"2"}"#,
            ),
        ]);

        let answering = async {
            fx.answer(Answered::AllowSession).await;
        };
        let turn = fx.turn(&provider);
        let plan = fx.plan();
        let cancel = CancellationToken::new();
        let (_reason, ()) = tokio::join!(turn.run(&plan, &cancel), answering);

        assert_eq!(fx.grants.list(&fx.session_id), vec![Grant::FsWrite]);
        assert!(fx.workspace.join("one.txt").is_file());
        assert!(
            fx.workspace.join("two.txt").is_file(),
            "the second write was covered by the grant"
        );

        let asked = fx
            .sink
            .names()
            .iter()
            .filter(|name| **name == "tool:approval_required")
            .count();
        assert_eq!(asked, 1, "the user was asked once, not twice");

        let audit = fx.audit.tail(10, None).expect("tail");
        assert_eq!(audit.len(), 2);
        // Newest first: the covered call, then the one that was approved.
        assert_eq!(audit[0].decision, AuditDecision::Auto);
        assert_eq!(audit[1].decision, AuditDecision::AllowSession);
    }

    /// Stopping a turn while its dialog is open must not leave the call
    /// unanswered, and must not record it as a refusal — nobody said no.
    #[tokio::test]
    async fn cancelling_while_waiting_abandons_the_call() {
        let fx = Fixture::new();
        fx.say("write a file");

        let provider = FakeProvider::scripted(vec![tool_call_script(
            "call_1",
            tool::FS_WRITE,
            r#"{"path":"new.txt","content":"x"}"#,
        )]);

        let cancel = CancellationToken::new();
        let stopping = async {
            fx.pending().await;
            cancel.cancel();
        };
        let turn = fx.turn(&provider);
        let plan = fx.plan();
        let (reason, ()) = tokio::join!(turn.run(&plan, &cancel), stopping);

        assert_eq!(reason, StopReason::Cancelled);
        assert!(!fx.workspace.join("new.txt").exists());
        assert!(
            fx.approvals.is_empty(),
            "a cancelled turn leaves no dialog behind"
        );

        let transcript = fx.transcript();
        assert_eq!(
            transcript[1].tool_calls[0].status,
            ToolCallStatus::Cancelled
        );
        let envelope: serde_json::Value =
            serde_json::from_str(&transcript[2].text).expect("an envelope");
        assert_eq!(envelope["error"]["code"], "E_CANCELLED");

        let resolved = fx.sink.events().into_iter().find_map(|event| match event {
            Event::ToolApprovalResolved(resolved) => Some(resolved),
            _ => None,
        });
        assert_eq!(
            resolved.expect("the dialog was closed").resolved_by,
            ResolvedBy::Policy
        );
    }

    /// An approval nobody answers is refused after [`APPROVAL_TTL`], and the
    /// turn carries on. Run on a paused clock, so the five minutes cost
    /// nothing: with every task idle, the runtime advances straight to the
    /// deadline the turn is waiting on.
    #[tokio::test(start_paused = true)]
    async fn an_unanswered_approval_expires_and_the_turn_carries_on() {
        let fx = Fixture::new();
        fx.say("write a file");

        let provider = FakeProvider::scripted(vec![tool_call_script(
            "call_1",
            tool::FS_WRITE,
            r#"{"path":"new.txt","content":"x"}"#,
        )]);

        let reason = fx
            .turn(&provider)
            .run(&fx.plan(), &CancellationToken::new())
            .await;

        assert_eq!(reason, StopReason::Stop, "the turn ends cleanly");
        assert!(
            !fx.workspace.join("new.txt").exists(),
            "an expired approval must not run the call"
        );
        assert!(fx.approvals.is_empty(), "the request was withdrawn");

        let transcript = fx.transcript();
        assert_eq!(transcript[1].tool_calls[0].status, ToolCallStatus::Denied);
        let envelope: serde_json::Value =
            serde_json::from_str(&transcript[2].text).expect("an envelope");
        assert_eq!(envelope["error"]["code"], "E_DENIED");

        let resolved = fx.sink.events().into_iter().find_map(|event| match event {
            Event::ToolApprovalResolved(resolved) => Some(resolved),
            _ => None,
        });
        assert_eq!(
            resolved.expect("the dialog was closed").resolved_by,
            ResolvedBy::Timeout
        );
    }

    /// While a dialog is open the session is not "working" — it is waiting for
    /// the person looking at it, and the sidebar has to say so.
    #[tokio::test]
    async fn the_session_reads_as_awaiting_approval_while_a_dialog_is_open() {
        let fx = Fixture::new();
        fx.say("write a file");

        let provider = FakeProvider::scripted(vec![tool_call_script(
            "call_1",
            tool::FS_WRITE,
            r#"{"path":"new.txt","content":"x"}"#,
        )]);

        let watching = async {
            fx.pending().await;
            let state = fx.turns.state_of(&fx.session_id);
            fx.answer(Answered::Deny).await;
            state
        };
        let turn = fx.turn(&provider);
        let plan = fx.plan();
        let cancel = CancellationToken::new();
        let (_reason, state) = tokio::join!(turn.run(&plan, &cancel), watching);

        assert_eq!(state, SessionState::AwaitingApproval);
        assert_eq!(
            fx.turns.state_of(&fx.session_id),
            SessionState::Running,
            "the turn goes back to working once it is answered"
        );

        let awaiting = fx.sink.events().into_iter().any(|event| match event {
            Event::SessionUpdated(summary) => summary.state == SessionState::AwaitingApproval,
            _ => false,
        });
        assert!(awaiting, "the sidebar was told the session is blocked");
    }

    /// PLAN 3.2: a path that will not resolve is refused outright, with no
    /// approval offered — approving it could not mean anything, because the
    /// dialog would have nothing true to show the user.
    ///
    /// A path *outside* the workspace is deliberately not the example here.
    /// That is an ask, not a denial, on every platform, and using it would
    /// leave this test waiting five minutes for an answer nobody gives.
    #[tokio::test]
    async fn a_hard_denial_is_a_result_and_the_turn_continues() {
        let fx = Fixture::new();
        fx.say("read a file whose name never resolves");

        let provider = FakeProvider::scripted(vec![tool_call_script(
            "call_1",
            tool::FS_READ,
            r#"{"path":"   "}"#,
        )]);

        let reason = fx
            .turn(&provider)
            .run(&fx.plan(), &CancellationToken::new())
            .await;

        assert_eq!(reason, StopReason::Stop, "a denial does not end the turn");
        assert!(
            !fx.sink.names().contains(&"tool:approval_required"),
            "a hard denial is never offered to the user"
        );

        let transcript = fx.transcript();
        assert_eq!(
            transcript[1].tool_calls[0].status,
            ToolCallStatus::Denied,
            "a refusal reads as refused, not as a call that ran and failed"
        );
        let envelope: serde_json::Value =
            serde_json::from_str(&transcript[2].text).expect("an envelope");
        assert_eq!(envelope["ok"], json!(false));

        // The refusal is audited: a call that never ran is still a call that
        // was made.
        let audit = fx.audit.tail(10, None).expect("tail");
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].outcome, Outcome::Denied);
    }

    /// Arguments that never became valid JSON are answered, never executed
    /// (PLAN 4.1) — the model can then correct itself.
    #[tokio::test]
    async fn unparseable_arguments_become_a_tool_message_not_a_dead_turn() {
        let fx = Fixture::new();
        fx.say("read something");

        let provider = FakeProvider::scripted(vec![tool_call_script(
            "call_1",
            tool::FS_READ,
            r#"{"path":"a.txt"#,
        )]);

        let reason = fx
            .turn(&provider)
            .run(&fx.plan(), &CancellationToken::new())
            .await;

        assert_eq!(reason, StopReason::Stop);
        assert!(!fx.sink.names().contains(&"tool:started"), "nothing ran");

        let transcript = fx.transcript();
        let envelope: serde_json::Value =
            serde_json::from_str(&transcript[2].text).expect("an envelope");
        assert_eq!(envelope["error"]["code"], "E_TOOL_FAILED");
        assert!(
            envelope["error"]["message"]
                .as_str()
                .expect("a message")
                .contains("not valid JSON"),
            "{envelope}"
        );
    }

    #[tokio::test]
    async fn a_provider_failure_ends_the_turn_with_an_error() {
        let fx = Fixture::new();
        fx.say("hello");

        let provider = FakeProvider::scripted(vec![vec![
            ModelEvent::TextDelta {
                text: "starting".to_owned(),
            },
            ModelEvent::Error {
                code: ErrorCode::ProviderHttp.as_str().to_owned(),
                message: "the provider answered 503".to_owned(),
                retryable: true,
            },
        ]]);

        let reason = fx
            .turn(&provider)
            .run(&fx.plan(), &CancellationToken::new())
            .await;

        assert_eq!(reason, StopReason::Error);
        assert_eq!(resting_state(reason), SessionState::Error);

        let error = fx
            .sink
            .events()
            .into_iter()
            .find_map(|event| match event {
                Event::TurnError(error) => Some(error),
                _ => None,
            })
            .expect("a turn:error");
        assert_eq!(error.code, "E_PROVIDER_HTTP");
        assert!(error.retryable);

        // `turn:error` is always followed by `turn:finished`, so a UI that only
        // tracks the lifecycle still re-enables its composer.
        assert_eq!(fx.sink.names().last(), Some(&"turn:finished"));
        assert!(fx
            .transcript()
            .last()
            .expect("partial")
            .text
            .contains("starting"));
    }

    /// A stream that stops without saying why produced a truncated reply.
    /// Reporting it as a clean stop would hide that.
    #[tokio::test]
    async fn a_stream_that_ends_without_finishing_is_an_error() {
        let fx = Fixture::new();
        fx.say("hello");

        let provider = FakeProvider::scripted(vec![vec![ModelEvent::TextDelta {
            text: "half a sen".to_owned(),
        }]]);

        let reason = fx
            .turn(&provider)
            .run(&fx.plan(), &CancellationToken::new())
            .await;

        assert_eq!(reason, StopReason::Error);
        let error = fx
            .sink
            .events()
            .into_iter()
            .find_map(|event| match event {
                Event::TurnError(error) => Some(error),
                _ => None,
            })
            .expect("a turn:error");
        assert_eq!(error.code, "E_PROVIDER_PARSE");
    }

    /// The ceiling of PLAN 4.2: the round after the cap is answered rather
    /// than executed, and the turn finishes cleanly.
    #[tokio::test]
    async fn the_tool_round_cap_ends_the_turn_cleanly() {
        let fx = Fixture::new();
        std::fs::write(fx.workspace.join("a.txt"), "x").expect("write");
        fx.say("keep reading");

        // One more round than the cap allows, so the last one is refused.
        let script = (0..=MAX_TOOL_ROUNDS)
            .map(|round| {
                tool_call_script(
                    &format!("call_{round}"),
                    tool::FS_READ,
                    r#"{"path":"a.txt"}"#,
                )
            })
            .collect();

        let provider = FakeProvider::scripted(script);
        let reason = fx
            .turn(&provider)
            .run(&fx.plan(), &CancellationToken::new())
            .await;

        assert_eq!(reason, StopReason::Stop);

        let transcript = fx.transcript();
        let last = transcript.last().expect("a final tool message");
        let envelope: serde_json::Value = serde_json::from_str(&last.text).expect("an envelope");
        assert_eq!(envelope["error"]["code"], "E_TOO_MANY_TOOL_ROUNDS");

        let executed = fx
            .sink
            .names()
            .iter()
            .filter(|name| **name == "tool:started")
            .count();
        assert_eq!(
            executed,
            usize::try_from(MAX_TOOL_ROUNDS).expect("small"),
            "the round past the cap is answered, not run"
        );
    }

    /// Every tool call must end up with a `tool` message, whatever happened —
    /// an unanswered one makes every later request in the session invalid.
    #[tokio::test]
    async fn every_call_is_answered_however_the_turn_ends() {
        let fx = Fixture::new();
        fx.say("do three things");

        let provider = FakeProvider::scripted(vec![vec![
            ModelEvent::ToolCallDelta {
                index: 0,
                id: Some("call_a".to_owned()),
                name: Some(tool::FS_LIST.to_owned()),
                args_delta: r#"{"path":"."}"#.to_owned(),
            },
            ModelEvent::ToolCallDelta {
                index: 1,
                id: Some("call_b".to_owned()),
                name: Some(tool::FS_READ.to_owned()),
                args_delta: r#"{"path":"nope.txt"}"#.to_owned(),
            },
            ModelEvent::Finish {
                reason: StopReason::ToolCalls,
                usage: None,
            },
        ]]);

        fx.turn(&provider)
            .run(&fx.plan(), &CancellationToken::new())
            .await;

        let transcript = fx.transcript();
        let answered: Vec<&str> = transcript
            .iter()
            .filter_map(|message| message.tool_call_id.as_deref())
            .collect();
        assert_eq!(answered, vec!["call_a", "call_b"]);
    }

    #[test]
    fn a_finished_turn_leaves_the_session_in_a_drawable_state() {
        assert_eq!(resting_state(StopReason::Stop), SessionState::Idle);
        assert_eq!(resting_state(StopReason::Cancelled), SessionState::Idle);
        assert_eq!(resting_state(StopReason::Length), SessionState::Idle);
        assert_eq!(resting_state(StopReason::Error), SessionState::Error);
    }
}
