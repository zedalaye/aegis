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
//! **A denial is a result.** Policy refusing a call, a user refusing one
//! (Phase 6), arguments that never parsed, the round cap — all of them become
//! an ordinary `tool` message with `ok: false`, and the turn continues. The
//! model reads it, explains itself and tries something else (PLAN 4.3). The
//! only things that end a turn early are cancellation and a provider failure.

use std::path::{Path, PathBuf};

use tokio::sync::mpsc;
use tokio::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

use crate::audit::{AuditDecision, AuditLog, Outcome};
use crate::error::ErrorCode;
use crate::policy::{self, Decision, GrantStore, PolicyCtx};
use crate::store::{
    Message, SessionState, SessionStore, SessionSummary, ToolCallRecord, ToolCallStatus,
};
use crate::tools::{self, ToolCtx, ToolOutcome, ToolResult};

use super::event::{
    Event, EventSink, ToolFinished, ToolRequested, ToolStarted, TurnDelta, TurnError, TurnFinished,
    TurnMessage, TurnStarted,
};
use super::provider::Provider;
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
    /// Where messages are read from and written to.
    pub sessions: &'a SessionStore,
    /// Live `allow_session` grants.
    pub grants: &'a GrantStore,
    /// Where tool calls are recorded.
    pub audit: &'a AuditLog,
    /// Who answers.
    pub provider: &'a dyn Provider,
    /// Where events go.
    pub sink: &'a dyn EventSink,
    /// This application's own binary, so `shell_exec` can refuse to run it.
    pub self_exe: Option<&'a Path>,
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

        let reason = loop {
            let history = match self.sessions.messages(&plan.session_id) {
                Ok(history) => history,
                Err(err) => {
                    // The session was deleted while its turn was running.
                    tracing::warn!(%err, session_id = %plan.session_id, "the turn lost its session");
                    break self.fail(plan, ErrorCode::Internal, &err.to_string(), false);
                }
            };

            let request = transcript::build(
                self.provider.model(),
                &history,
                plan.workspace.as_deref(),
                tools::schemas(),
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
                        self.refuse_all(plan, &calls);
                        break StopReason::Stop;
                    }

                    self.execute(plan, &calls, cancel);
                    rounds += 1;

                    if cancel.is_cancelled() {
                        break StopReason::Cancelled;
                    }
                }
            }
        };

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
    /// Sequential rather than concurrent: the calls mutate a filesystem, the
    /// user approves them one at a time from Phase 6, and two writes to the
    /// same path racing each other is not a behaviour worth having.
    ///
    /// Synchronous, because everything it can currently run is. The filesystem
    /// tools are short and local, so blocking the worker for one is cheaper
    /// than moving it to another thread. Phase 7's `shell_exec` is the first
    /// call that genuinely waits, and it is what makes this `async` again.
    fn execute(&self, plan: &TurnPlan, calls: &[AssembledCall], cancel: &CancellationToken) {
        for call in calls {
            if cancel.is_cancelled() {
                self.abandon(plan, call);
                continue;
            }

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

            let ctx = ToolCtx {
                session_id: &plan.session_id,
                turn_id: &plan.turn_id,
                call_id: &call.call_id,
                audit: self.audit,
                args: &args,
            };
            let policy_ctx =
                PolicyCtx::new(&plan.session_id, plan.workspace.as_deref(), self.grants)
                    .with_self_exe(self.self_exe);

            let outcome = match policy::decide(&policy_ctx, &call.name, args.clone()) {
                Decision::Auto {
                    call: resolved,
                    reason,
                } => {
                    self.sink.emit(Event::ToolStarted(ToolStarted {
                        session_id: plan.session_id.clone(),
                        turn_id: plan.turn_id.clone(),
                        call_id: call.call_id.clone(),
                        tool: call.name.clone(),
                    }));
                    // Filesystem work is short and local, so it runs inline
                    // rather than on a blocking thread. Phase 7's `shell_exec`
                    // is the one that genuinely waits, and it spawns.
                    tools::run(&ctx, AuditDecision::Auto, reason, &resolved)
                }
                Decision::Deny { code, reason } => {
                    tools::refuse(&ctx, &call.name, AuditDecision::Deny, code, &reason)
                }
                // Phase 6 replaces this arm with the approval registry: it
                // emits `tool:approval_required`, parks the turn on a oneshot
                // and resumes on the user's answer. Until then the honest
                // answer is that nothing can approve it, said in terms the
                // model can act on rather than a silent hang.
                Decision::Ask { request, .. } => tools::refuse(
                    &ctx,
                    &call.name,
                    AuditDecision::Deny,
                    ErrorCode::Denied,
                    &format!(
                        "this call needs approval ({}), and the approval gate is not wired up in \
                         this build",
                        request.reason
                    ),
                ),
            };

            let status = if outcome.result.ok {
                ToolCallStatus::Ok
            } else {
                ToolCallStatus::Error
            };
            self.finish_call(plan, call, &outcome, status);
        }
    }

    /// Answers every call of a round without running any of them.
    ///
    /// Used for the round cap: the model has to see one `tool` message per
    /// call it made, or the next request it appears in is structurally
    /// invalid (see [`transcript`]).
    fn refuse_all(&self, plan: &TurnPlan, calls: &[AssembledCall]) {
        for call in calls {
            let reason = format!(
                "this turn already ran {MAX_TOOL_ROUNDS} rounds of tools, which is the limit; \
                 answer with what you have, or ask the user to continue"
            );
            let ctx = ToolCtx {
                session_id: &plan.session_id,
                turn_id: &plan.turn_id,
                call_id: &call.call_id,
                audit: self.audit,
                args: call.args.as_ref().unwrap_or(&serde_json::Value::Null),
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
        }));
        self.sink
            .emit(Event::AuditAppended(Box::new(outcome.audit.clone())));

        self.answer(
            plan,
            call,
            outcome.result.clone(),
            status,
            outcome.summary.clone(),
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
    ) {
        if let Err(err) = self.sessions.set_tool_call_status(
            &plan.session_id,
            &call.call_id,
            status,
            Some(summary),
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
    use crate::policy::tool;

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
        grants: GrantStore,
        audit: AuditLog,
        sink: Recorder,
        session_id: String,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = TempDir::new().expect("temp dir");
            let data = dir.path().join("data");
            let workspace = dir.path().join("work");
            std::fs::create_dir_all(&data).expect("data dir");
            std::fs::create_dir_all(&workspace).expect("workspace dir");

            let sessions = SessionStore::load(&data);
            let session_id = sessions.create("p1", None).expect("session").id;

            Self {
                workspace: dunce::canonicalize(&workspace).expect("canonical workspace"),
                _dir: dir,
                sessions,
                grants: GrantStore::new(),
                audit: AuditLog::new(&data),
                sink: Recorder::default(),
                session_id,
            }
        }

        fn plan(&self) -> TurnPlan {
            TurnPlan {
                session_id: self.session_id.clone(),
                turn_id: "t1".to_owned(),
                workspace: Some(self.workspace.clone()),
            }
        }

        fn turn<'a>(&'a self, provider: &'a dyn Provider) -> Turn<'a> {
            Turn {
                sessions: &self.sessions,
                grants: &self.grants,
                audit: &self.audit,
                provider,
                sink: &self.sink,
                self_exe: None,
            }
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

    /// A read inside the workspace is auto-allowed; a write is not, and until
    /// Phase 6 there is nothing that can approve it. The turn must survive
    /// that as an ordinary refusal rather than hanging.
    #[tokio::test]
    async fn a_call_needing_approval_is_refused_rather_than_hanging() {
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

        assert_eq!(reason, StopReason::Stop);
        assert!(
            !fx.workspace.join("new.txt").exists(),
            "an unapproved write must not touch the disk"
        );

        let transcript = fx.transcript();
        let envelope: serde_json::Value =
            serde_json::from_str(&transcript[2].text).expect("an envelope");
        assert_eq!(envelope["ok"], json!(false));
        assert_eq!(envelope["error"]["code"], "E_DENIED");
    }

    #[tokio::test]
    async fn a_hard_denial_is_a_result_and_the_turn_continues() {
        let fx = Fixture::new();
        fx.say("read the password file");

        let outside = if cfg!(windows) {
            r#"{"path":"C:\\Windows\\win.ini"}"#
        } else {
            r#"{"path":"/etc/passwd"}"#
        };
        let provider =
            FakeProvider::scripted(vec![tool_call_script("call_1", tool::FS_READ, outside)]);

        let reason = fx
            .turn(&provider)
            .run(&fx.plan(), &CancellationToken::new())
            .await;

        assert_eq!(reason, StopReason::Stop, "a denial does not end the turn");

        let transcript = fx.transcript();
        assert_eq!(transcript[1].tool_calls[0].status, ToolCallStatus::Error);
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
