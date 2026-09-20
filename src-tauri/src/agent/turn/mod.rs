//! The turn state machine (PLAN 4.2).
//!
//! ```text
//! session_send
//!   └─> Turn { id, session_id, cancel }
//!       Building -> Streaming -> [ToolPending -> Executing -> Building]* -> Done
//! ```
//!
//! One function owns the whole shape: a tool result is persisted before the
//! next request, the assistant message before its calls run, and every exit
//! leaves the session idle and the UI told.
//!
//! * **Cancellation is checked at every await**; text already streamed is kept.
//! * **Deltas are coalesced** into ~50 ms frames opened by the first token.
//! * **A denial is a result**: refusals, unanswered approvals, unparsed
//!   arguments and a loop or round-ceiling halt become `tool` messages, and the
//!   turn continues (PLAN 4.3). Only cancellation and provider failure end it
//!   early. A halt gives the model one wrap-up round to finish; it is not told
//!   to ask the user to continue (PLAN 7.16).
//! * **Waiting for a person is a state**: the session reads `awaiting_approval`,
//!   inside the same `select!` as cancel and under a five-minute deadline.
//! * [`Standing`] (Phase 15) and [`Unattended`] (Phase 16) are the only ways a
//!   session, a delegated brief and a routine's run differ. There is no second
//!   loop.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

use crate::approval::{Answer, ApprovalRegistry, Decision as Answered, ResolvedBy, APPROVAL_TTL};
use crate::audit::{AuditDecision, AuditLog, Outcome};
use crate::compact;
use crate::error::ErrorCode;
use crate::exec_host::{self, ExecHost};
use crate::handoff::{self, bus};
use crate::mcp::{self, Connectors};
use crate::policy::{self, AskRequest, Decision, GrantStore, Identity, PolicyCtx};
use crate::skills::{self, SkillCtx};
use crate::store::memories::{self, MemoryStore};
use crate::store::{
    Agent, Message, SessionState, SessionStore, SessionSummary, ToolCallRecord, ToolCallStatus,
    TurnCost,
};
use crate::tools::handoff::HandoffCtx;
use crate::tools::{self, NullProgress, ProgressSink, Stream, ToolCtx, ToolOutcome, ToolResult};
use crate::workspace;
use crate::world;

use super::decision::{tool_risk, DecisionClient};
use super::event::{
    Event, EventSink, ToolApprovalAnnotated, ToolApprovalResolved, ToolDrafting, ToolFinished,
    ToolProgress, ToolRequested, ToolStarted, TurnDelta, TurnError, TurnFinished, TurnMessage,
    TurnStarted,
};
use super::guard::{self, Halt};
use super::provider::Provider;
use super::registry::{TurnRegistry, MAX_RUN_TURNS};
use super::transcript;
use super::wire::{AssembledCall, ModelEvent, StopReason, Usage};

pub use super::guard::{LOOP_STREAK, MAX_TOOL_ROUNDS};

mod call;
mod stream;

/// How long a `turn:delta` frame stays open.
pub const DELTA_FRAME: Duration = Duration::from_millis(50);

/// A tool call the model is still writing: enough to show that something is
/// happening while no text streams.
#[derive(Debug)]
struct Drafting {
    /// Which call within this response.
    index: u32,
    /// The tool, once the model has named it.
    tool: Option<String>,
    /// Argument bytes seen so far.
    bytes: u64,
    /// What the last emitted event said, so an unchanged count stays quiet.
    reported: u64,
}

impl Drafting {
    const fn new(index: u32) -> Self {
        Self {
            index,
            tool: None,
            bytes: 0,
            reported: 0,
        }
    }
}

/// Where a turn sits in the Chef-de-Cabinet loop (Phase 15): it may hand work
/// out, or it is handed-out work — never both (`COS.md` *Roles*). Decides the
/// tools offered, what [`policy::decide`] refuses, and whether a filed report
/// ends the turn.
pub enum Standing<'a> {
    /// A session someone opened, which may delegate. `None` is no bus (a test),
    /// and `handoff_delegate` is then not offered.
    Own(Option<&'a Arc<dyn bus::Runner>>),
    /// A run a brief opened. It may not delegate, and it answers by filing a
    /// report into this cell.
    Delegated(&'a handoff::Open),
}

impl Standing<'_> {
    /// The delegated run this turn is, if it is one.
    const fn open(&self) -> Option<&handoff::Open> {
        match self {
            Self::Own(_) => None,
            Self::Delegated(open) => Some(open),
        }
    }

    /// What a tool call in this turn is given.
    const fn ctx(&self) -> HandoffCtx<'_> {
        match self {
            Self::Own(bus) => HandoffCtx {
                bus: *bus,
                open: None,
            },
            Self::Delegated(open) => HandoffCtx {
                bus: None,
                open: Some(open),
            },
        }
    }
}

/// The routine a turn runs for, when a clock started it (Phase 16).
///
/// Orthogonal to [`Standing`]: a routine's run is nobody's specialist, and
/// nobody is in front of it. It changes the system message, turns asks into
/// refusals ([`policy`]), tags audit lines, and carries the cell the run's
/// report lands in for the scheduler.
#[derive(Debug, Clone, Copy)]
pub struct Unattended<'a> {
    /// The routine's id. Reaches every audit line the run writes.
    pub routine: &'a str,
    /// Where the run's `skill_return` is left for whoever started it.
    pub reported: &'a skills::Reported,
}

/// The tools this turn's identity holds here.
///
/// A delegated run gains `handoff_return` (its only way to answer) and loses
/// `handoff_delegate`; an ordinary session loses `handoff_return`, which could
/// only fail. The built-in identity gains every connector tool, since it holds
/// "every tool this build has"; any other identity holds exactly what it was
/// granted (AGENTS.md). Connector calls are asked about either way.
fn held(agent: &Agent, standing: &Standing<'_>, connectors: &mcp::Catalog) -> Vec<String> {
    let (dropped, added) = match standing {
        Standing::Own(_) => (policy::tool::HANDOFF_RETURN, None),
        Standing::Delegated(_) => (
            policy::tool::HANDOFF_DELEGATE,
            Some(policy::tool::HANDOFF_RETURN),
        ),
    };

    let mut held: Vec<String> = agent
        .tools
        .iter()
        .filter(|name| name.as_str() != dropped)
        .cloned()
        .collect();

    if let Some(added) = added {
        if !held.iter().any(|name| name == added) {
            held.push(added.to_owned());
        }
    }

    if agent.builtin {
        for name in connectors.names() {
            if !held.contains(&name) {
                held.push(name);
            }
        }
    }
    held
}

/// The allow-list as shown to the model: without a decision client, the
/// `jev_*` tools could only fail after a dialog, so they are left out.
fn offered_tools(held: &[String], decision: bool) -> Vec<String> {
    held.iter()
        .filter(|name| {
            decision
                || !matches!(
                    name.as_str(),
                    policy::tool::JEV_EVAL | policy::tool::JEV_ASK
                )
        })
        .cloned()
        .collect()
}

/// What this turn offered the model: the allow-list and the connector catalog
/// it was resolved against, settled together at the top of [`Turn::run`].
#[derive(Clone, Copy)]
struct Offered<'a> {
    /// The tools this identity holds, here.
    held: &'a [String],
    /// The connector tools that were callable when the turn began.
    connectors: &'a mcp::Catalog,
}

/// What a turn needs to know about itself.
#[derive(Debug, Clone)]
pub struct TurnPlan {
    /// The session it belongs to.
    pub session_id: String,
    /// This turn, unique within the process.
    pub turn_id: String,
    /// The canonical workspace root. `None` when the folder is gone: every call
    /// is then `E_NO_WORKSPACE`, and the system message says so.
    pub workspace: Option<PathBuf>,
    /// Where this project's commands run (PLAN 7.12), fixed for the turn so the
    /// system message and `shell_exec` agree. Only `shell_exec` reads it.
    pub exec_host: Option<ExecHost>,
}

/// Everything the loop borrows for one turn, built from
/// [`AppState`](crate::state::AppState).
pub struct Turn<'a> {
    /// The identity this turn runs as (Phase 12), resolved once so the tools
    /// shown and the tools judged cannot differ. Read by the system message,
    /// the schemas and policy, and nothing else.
    pub agent: &'a Agent,
    /// Where messages are read from and written to.
    pub sessions: &'a SessionStore,
    /// Which sessions are running; the turn marks its session as waiting on an
    /// approval here.
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
    /// Where `screen_capture` writes, never the workspace (PLAN 5.4).
    pub captures: &'a Path,
    /// The skill library (Phase 13); the workspace is the other place runbooks
    /// live.
    pub skills: &'a Path,
    /// The memory store (Phase 14): read at the top of the turn, written by
    /// `memory_write`, always scoped to [`Turn::agent`].
    pub memories: &'a MemoryStore,
    /// Where this turn sits (Phase 15), resolved before the first request.
    pub standing: Standing<'a>,
    /// The routine this turn runs for (Phase 16). `None` is a session with a
    /// person in front of it.
    pub unattended: Option<Unattended<'a>>,
    /// The running connectors (Phase 18). [`Connectors::new`] is an empty
    /// roster.
    pub connectors: &'a Connectors,
    /// The decision client (PLAN 7.18), for `jev_*` calls and the approval
    /// annotation. `None` without a TypeSafe key: those tools are then not
    /// offered, and dialogs open unannotated.
    pub decision: Option<&'a DecisionClient>,
}

/// The [`ProgressSink`] one tool call writes to. The turn, not the tool,
/// numbers the frames, so the UI can drop duplicates with one rule.
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
    /// The routine id for audit lines; empty for a session someone opened.
    fn routine(&self) -> &str {
        self.unattended.map_or("", |run| run.routine)
    }

    /// Runs a turn to completion. Never an `Err`: a failure is a tool result
    /// for the model or a `turn:error` for the user.
    pub async fn run(&self, plan: &TurnPlan, cancel: &CancellationToken) -> StopReason {
        self.sink.emit(Event::TurnStarted(TurnStarted {
            session_id: plan.session_id.clone(),
            turn_id: plan.turn_id.clone(),
            model: self.provider.model().to_owned(),
        }));

        let mut seq = 0u32;
        let mut rounds = 0u32;
        let mut fingerprints: Vec<String> = Vec::new();
        // A halt refuses the pending round and then lets the model speak once
        // more. A second tool round after that stops; the wrap-up is not a
        // new budget.
        let mut wrapping: Option<Halt> = None;
        let mut usage: Option<Usage> = None;

        // Whether this turn is a brief: the same fact policy reads, so the world
        // frame and the gate agree about writing `world/`.
        let delegated = self.standing.open().is_some();

        // The catalog, once per turn so the list shown and the list judged
        // match. Bodies load on `skill_run` (PLAN 7.6).
        let catalog = skills::catalog(self.skills, plan.workspace.as_deref());
        let offered = skills::granted(&catalog, self.agent);
        let skill_block = skills::prompt_block(&offered);

        // What this identity holds here, with the connector catalog it was
        // resolved against: one snapshot per turn, read by the schemas, policy
        // and the runbook check alike.
        let connectors = self.connectors.catalog();
        let held = held(self.agent, &self.standing, &connectors);
        let offered = Offered {
            held: &held,
            connectors: &connectors,
        };

        // The run this session is following, seeded from the session so a run
        // the round cap interrupted resumes (`IDEAS.md` § 10).
        let mut skill: Option<String> = self.turns.open_run(&plan.session_id);

        // Fold once per turn, before the first request, never between rounds.
        // Nothing is lost (`COS.md` *Memory*): the transcript stays whole, and
        // memories are rebuilt below.
        if let Err(err) = self.sessions.compact(&plan.session_id, false) {
            // A session that could not fold is expensive, not broken.
            tracing::warn!(%err, session_id = %plan.session_id, "could not compact the session");
        }

        // Once per turn; a memory this turn writes reaches the next one.
        let remembered = self.memories.list_for(&self.agent.id);
        let memory_block = memories::prompt_block(&remembered, remembered.len());

        // Once per turn, like the host itself.
        let host_block = plan
            .exec_host
            .as_ref()
            .map(|host| exec_host::prompt_block(host, plan.workspace.as_deref()));

        // One progress counter for every call in the turn. Atomic: tools get
        // the sink as `&dyn`.
        let progress_seq = AtomicU32::new(0);

        let reason = loop {
            // Transcript and fold under one lock, so they match.
            let (history, compaction) = match self.sessions.context(&plan.session_id) {
                Ok(context) => context,
                Err(err) => {
                    // The session was deleted while its turn was running.
                    tracing::warn!(%err, session_id = %plan.session_id, "the turn lost its session");
                    break self.fail(plan, ErrorCode::Internal, &err.to_string(), false);
                }
            };
            // What still reaches the model. The whole transcript for a session
            // that has never folded; the raw tail for one that has.
            let raw = compact::tail(
                &history,
                compaction
                    .as_ref()
                    .map(|held| held.through_message_id.as_str()),
            );

            // Every round (Phase 11): a round that wrote `DECISIONS.md` sees it
            // in the next.
            let shared = plan.workspace.as_deref().and_then(workspace::digest);

            // Every round too (PLAN 7.2): a declared source can move mid-turn.
            let world = plan
                .workspace
                .as_deref()
                .and_then(|root| world::block(root, delegated));

            let request = transcript::build(
                self.provider.model(),
                &transcript::Context {
                    agent: self.agent,
                    workspace: plan.workspace.as_deref(),
                    exec_host: host_block.as_deref(),
                    memories: memory_block.as_deref(),
                    skills: skill_block.as_deref(),
                    world: world.as_deref(),
                    shared: shared.as_deref(),
                    compacted: compaction.as_ref().map(|held| held.state.as_str()),
                    unattended: self.unattended.is_some(),
                },
                raw,
                // Only tools the identity holds are shown; policy still refuses
                // a replayed call. The decision tools need a client to be
                // worth a dialog.
                tools::schemas_for(&offered_tools(&held, self.decision.is_some()), &connectors),
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
                    // Summed over rounds: each request was paid for.
                    if let Some(round) = reported {
                        match &mut usage {
                            Some(spent) => spent.add(round),
                            None => usage = Some(round),
                        }
                    }

                    let records: Vec<ToolCallRecord> = calls.iter().map(record_of).collect();
                    self.persist_assistant(plan, text, records);

                    if calls.is_empty() {
                        break reason;
                    }

                    let incoming = guard::fingerprint(&calls);
                    let halt = wrapping.or_else(|| guard::halt(rounds, &fingerprints, &incoming));
                    if let Some(halt) = halt {
                        tracing::warn!(
                            session_id = %plan.session_id,
                            rounds,
                            ?halt,
                            skill = skill.as_deref().unwrap_or(""),
                            "tool round halted"
                        );
                        self.refuse_all(plan, &calls, &held, skill.as_deref(), halt);
                        if wrapping.is_some() {
                            // Already had the wrap-up request and still called
                            // tools. Stop rather than refuse forever.
                            break StopReason::Stop;
                        }
                        wrapping = Some(halt);
                        continue;
                    }

                    self.execute(plan, &calls, offered, cancel, &progress_seq, &mut skill)
                        .await;
                    fingerprints.push(incoming);
                    rounds += 1;

                    if cancel.is_cancelled() {
                        break StopReason::Cancelled;
                    }

                    // A returned brief ends the turn: a delegation costs no
                    // more than its report.
                    if self.standing.open().is_some_and(handoff::Open::closed) {
                        tracing::debug!(turn_id = %plan.turn_id, "the brief was returned");
                        break StopReason::Stop;
                    }
                }
            }
        };

        // An unreturned run carries to the next turn, unless the turn was
        // cancelled (Stop means the conversation moved on) or the run is past
        // the ceiling in `registry`.
        let carried = match reason {
            StopReason::Cancelled => None,
            _ => skill.as_deref(),
        };
        match (carried, self.turns.carry_run(&plan.session_id, carried)) {
            (Some(unfinished), Some(turns)) => tracing::debug!(
                session_id = %plan.session_id,
                turn_id = %plan.turn_id,
                skill = %unfinished,
                turns,
                "the turn ended mid-run; it carries to the next one"
            ),
            (Some(dropped), None) => tracing::warn!(
                session_id = %plan.session_id,
                turn_id = %plan.turn_id,
                skill = %dropped,
                limit = MAX_RUN_TURNS,
                "a run spent its turns without a skill_return and was dropped"
            ),
            (None, _) => {}
        }

        // An unreturned brief: the runner counts a failed attempt. Logged, since
        // "the model just stopped" is otherwise invisible.
        if self.standing.open().is_some_and(|open| !open.closed()) {
            tracing::warn!(
                session_id = %plan.session_id,
                turn_id = %plan.turn_id,
                ?reason,
                "a delegated turn ended without a handoff_return"
            );
        }

        // What the turn spent (Phase 17), keyed by turn id, for every ending —
        // unknown rather than zero when the provider said nothing. A failure to
        // record it is logged, not fatal.
        let charge = match usage {
            Some(spent) => {
                TurnCost::reported(&plan.turn_id, spent.prompt_tokens, spent.completion_tokens)
                    .with_cache(spent.cache_read_tokens, spent.cache_creation_tokens)
            }
            None => TurnCost::unreported(&plan.turn_id),
        };
        if let Err(err) = self.sessions.charge(&plan.session_id, charge) {
            tracing::warn!(
                %err,
                session_id = %plan.session_id,
                turn_id = %plan.turn_id,
                "a turn's cost could not be recorded"
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
    // Persistence
    // -----------------------------------------------------------------------

    /// Persists this round's assistant message, unless it is empty.
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
                self.sink.emit(Event::SessionUpdated(Box::new(summary)));
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

/// What the model is told about a call that did not run: what happened, and
/// what to do next, so it does not retry.
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

/// The transcript record for a new call, `Pending` until policy decides.
fn record_of(call: &AssembledCall) -> ToolCallRecord {
    ToolCallRecord {
        call_id: call.call_id.clone(),
        tool: call.name.clone(),
        args_json: call.args_json.clone(),
        status: ToolCallStatus::Pending,
        summary: None,
        // Filled in when the call finishes, and only by `screen_capture`.
        image_path: None,
        thought_signature: call.thought_signature.clone(),
    }
}

/// The state a session rests in once its turn has ended, for the command
/// layer.
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
mod tests;
