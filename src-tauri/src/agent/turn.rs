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

use super::event::{
    Event, EventSink, ToolApprovalResolved, ToolDrafting, ToolFinished, ToolProgress,
    ToolRequested, ToolStarted, TurnDelta, TurnError, TurnFinished, TurnMessage, TurnStarted,
};
use super::guard::{self, Halt};
use super::provider::Provider;
use super::registry::{TurnRegistry, MAX_RUN_TURNS};
use super::transcript;
use super::wire::{AssembledCall, ModelEvent, StopReason, Usage};

pub use super::guard::{LOOP_STREAK, MAX_TOOL_ROUNDS};

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
                // a replayed call.
                tools::schemas_for(&held, &connectors),
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
    // Streaming
    // -----------------------------------------------------------------------

    /// Consumes one response, coalescing text and assembling tool calls.
    /// `biased`, so a cancel is polled before buffered events.
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
        // A call being written, reported by size only.
        let mut drafting: Option<Drafting> = None;

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
                    self.flush(plan, &mut frame, drafting.as_mut(), seq);
                    tracing::debug!(turn_id = %plan.turn_id, "cancelled mid-stream");
                    return Streamed::Cancelled { text };
                }

                () = tick => {
                    self.flush(plan, &mut frame, drafting.as_mut(), seq);
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
                        ModelEvent::ToolCallDelta {
                            index,
                            id,
                            name,
                            args_delta,
                            thought_signature,
                        } => {
                            // Measured before `push` takes the fragment.
                            let grown = args_delta.len() as u64;
                            let draft = drafting.get_or_insert_with(|| Drafting::new(index));
                            if draft.index != index {
                                // A second call: flush the first one's size.
                                self.emit_drafting(plan, draft, seq);
                                *draft = Drafting::new(index);
                            }
                            if let Some(name) = &name {
                                draft.tool = Some(name.clone());
                            }
                            draft.bytes = draft.bytes.saturating_add(grown);

                            assembler.push_signed(
                                index,
                                id,
                                name,
                                &args_delta,
                                thought_signature,
                            );

                            // Arguments with no text still open a frame.
                            if deadline.is_none() {
                                deadline = Some(Instant::now() + DELTA_FRAME);
                            }
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

        self.flush(plan, &mut frame, drafting.as_mut(), seq);

        if let Some((code, message, retryable)) = failure {
            return Streamed::Failed {
                text,
                code,
                message,
                retryable,
            };
        }

        let Some((reason, usage)) = reason else {
            // Closed without `Finish`: a truncated reply, not a short success.
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

    /// Emits the frame's text and the drafting call's size together, and
    /// empties the frame.
    fn flush(
        &self,
        plan: &TurnPlan,
        frame: &mut String,
        drafting: Option<&mut Drafting>,
        seq: &mut u32,
    ) {
        if !frame.is_empty() {
            self.sink.emit(Event::TurnDelta(TurnDelta {
                session_id: plan.session_id.clone(),
                turn_id: plan.turn_id.clone(),
                seq: *seq,
                text: std::mem::take(frame),
            }));
            *seq = seq.saturating_add(1);
        }

        if let Some(draft) = drafting {
            self.emit_drafting(plan, draft, seq);
        }
    }

    /// Reports how far a call's arguments have got, only when that changed.
    fn emit_drafting(&self, plan: &TurnPlan, draft: &mut Drafting, seq: &mut u32) {
        if draft.bytes == draft.reported {
            return;
        }

        self.sink.emit(Event::ToolDrafting(ToolDrafting {
            session_id: plan.session_id.clone(),
            turn_id: plan.turn_id.clone(),
            index: draft.index,
            tool: draft.tool.clone(),
            seq: *seq,
            bytes: draft.bytes,
        }));
        draft.reported = draft.bytes;
        *seq = seq.saturating_add(1);
    }

    // -----------------------------------------------------------------------
    // Tools
    // -----------------------------------------------------------------------

    /// Runs one round of tool calls in order, one at a time, because a person
    /// approves them one at a time. `async` for [`Turn::ask`] and `shell_exec`.
    async fn execute(
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
            };

            // Only a capture needs the display geometry, passed in so policy
            // stays pure.
            let screen = (call.name == policy::tool::SCREEN_CAPTURE)
                .then(tools::screenshot::geometry)
                .flatten();
            let policy_ctx =
                PolicyCtx::new(&plan.session_id, plan.workspace.as_deref(), self.grants)
                    .with_self_exe(self.self_exe)
                    .with_exec_host(plan.exec_host.as_ref())
                    .with_screen(screen.as_ref())
                    .with_connectors(Some(connectors))
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

    /// Parks the turn until a person answers. `None` means the turn was
    /// cancelled, which is not a denial. On every exit the request is
    /// withdrawn, the session stops waiting, and `tool:approval_resolved` is
    /// emitted, so no dialog outlives its call.
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

        // `biased`: a Stop that lands alongside an answer wins.
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

    /// Emits `tool:started` for a call that policy — or the user — cleared.
    fn starting(&self, plan: &TurnPlan, call: &AssembledCall) {
        self.sink.emit(Event::ToolStarted(ToolStarted {
            session_id: plan.session_id.clone(),
            turn_id: plan.turn_id.clone(),
            call_id: call.call_id.clone(),
            tool: call.name.clone(),
        }));
    }

    /// Re-sends the session's row at the registry's state, so the sidebar and
    /// `session_open` agree.
    fn session_changed(&self, plan: &TurnPlan) {
        let state = self.turns.state_of(&plan.session_id);
        if let Some(summary) = summarize(self.sessions, &plan.session_id, state) {
            self.sink.emit(Event::SessionUpdated(Box::new(summary)));
        }
    }

    /// Answers every call of a round without running it (a loop or the round
    /// ceiling). Every call needs a `tool` message, or the next request is
    /// invalid ([`transcript`]).
    fn refuse_all(
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
            };
            let outcome =
                tools::refuse(&ctx, &call.name, AuditDecision::Deny, halt.code(), &reason);
            self.finish_call(plan, call, &outcome, ToolCallStatus::Denied);
        }
    }

    /// Records a call a cancel arrived before. Still answered, so later
    /// requests stay valid.
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
            Ok(summary) => self.sink.emit(Event::SessionUpdated(Box::new(summary))),
            Err(err) => {
                tracing::warn!(%err, call_id = %call.call_id, "could not record the tool result");
            }
        }
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
        /// An empty skill library; the runner is tested elsewhere.
        library: PathBuf,
        /// An empty memory store.
        memories: MemoryStore,
        connectors: Connectors,
        /// The built-in identity, which holds every tool.
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

            // Registered as `session_send` does; tests read the state back.
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
                memories: MemoryStore::load(&data),
                connectors: Connectors::new(),
                agent: Agent::builtin(),
            }
        }

        fn plan(&self) -> TurnPlan {
            TurnPlan {
                session_id: self.session_id.clone(),
                turn_id: TURN_ID.to_owned(),
                workspace: Some(self.workspace.clone()),
                exec_host: None,
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
                memories: &self.memories,
                connectors: &self.connectors,
                standing: Standing::Own(None),
                unattended: None,
            }
        }

        /// The approval the turn is blocked on, polled the way a click finds it.
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
                thought_signature: None,
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

    /// An unanswered approval is refused after [`APPROVAL_TTL`] and the turn
    /// carries on. A paused clock skips the wait.
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

    /// PLAN 3.2: an unresolvable path is refused with no approval, and the turn
    /// continues. (A path outside the workspace would ask, not refuse.)
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

    /// PLAN 7.16: repeating the same call is a loop, not a request for the
    /// user to type continue. The third identical round is refused; the two
    /// before it ran.
    #[tokio::test]
    async fn a_repeated_call_stops_as_a_loop() {
        let fx = Fixture::new();
        std::fs::write(fx.workspace.join("a.txt"), "x").expect("write");
        fx.say("keep reading");

        // LOOP_STREAK identical rounds, plus one more the wrap-up must not run.
        let script = (0..=LOOP_STREAK)
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

        let envelopes: Vec<serde_json::Value> = fx
            .transcript()
            .into_iter()
            .filter(|message| message.role == crate::store::Role::Tool)
            .map(|message| serde_json::from_str(&message.text).expect("an envelope"))
            .collect();
        let looped = envelopes
            .iter()
            .filter(|envelope| envelope["error"]["code"] == "E_TOOL_LOOP")
            .count();
        assert!(
            looped >= 1,
            "the repeated round is refused as a loop: {envelopes:?}"
        );
        assert!(
            envelopes.iter().all(|envelope| {
                envelope["error"]["code"] != "E_TOOL_LOOP"
                    || !envelope["error"]["message"]
                        .as_str()
                        .unwrap_or_default()
                        .to_lowercase()
                        .contains("continue")
            }),
            "the recovery is finish, not a human continue: {envelopes:?}"
        );

        let executed = fx
            .sink
            .names()
            .iter()
            .filter(|name| **name == "tool:started")
            .count();
        assert_eq!(
            executed,
            usize::try_from(LOOP_STREAK.saturating_sub(1)).expect("small"),
            "only the rounds before the loop ran"
        );
    }

    /// Distinct work past the old eight-round cap keeps running. The ceiling
    /// is a bill bound, not "stop and wait for continue".
    #[tokio::test]
    async fn progress_past_the_old_cap_keeps_running() {
        let fx = Fixture::new();
        fx.say("read them");

        let old_cap = 8u32;
        let past = old_cap + 2;
        for n in 0..past {
            std::fs::write(fx.workspace.join(format!("a{n}.txt")), "x").expect("write");
        }
        let script = (0..past)
            .map(|n| {
                tool_call_script(
                    &format!("call_{n}"),
                    tool::FS_READ,
                    &format!(r#"{{"path":"a{n}.txt"}}"#),
                )
            })
            .collect();

        let provider = FakeProvider::scripted(script);
        fx.turn(&provider)
            .run(&fx.plan(), &CancellationToken::new())
            .await;

        let refused = fx.transcript().into_iter().any(|message| {
            serde_json::from_str::<serde_json::Value>(&message.text).is_ok_and(|envelope| {
                envelope["error"]["code"] == "E_TOO_MANY_TOOL_ROUNDS"
                    || envelope["error"]["code"] == "E_TOOL_LOOP"
            })
        });
        assert!(!refused, "ten distinct reads are progress, not a halt");

        let executed = fx
            .sink
            .names()
            .iter()
            .filter(|name| **name == "tool:started")
            .count();
        assert_eq!(
            executed,
            usize::try_from(past).expect("small"),
            "every distinct round ran"
        );
    }

    /// Writes a runbook into the library and grants it: writing alone grants
    /// nothing.
    fn grant_runbook(fx: &mut Fixture, name: &str) {
        let dir = fx.library.join(name);
        std::fs::create_dir_all(&dir).expect("skill dir");
        std::fs::write(dir.join("SKILL.md"), crate::skills::TRIAGE_SEED).expect("runbook");
        fx.agent.skills = vec![name.to_owned()];
    }

    /// After a loop halt the model gets one wrap-up request, so it can
    /// summarize instead of leaving the user to type continue (PLAN 7.16).
    #[tokio::test]
    async fn a_halt_gives_the_model_a_wrap_up_round() {
        let fx = Fixture::new();
        std::fs::write(fx.workspace.join("a.txt"), "x").expect("write");
        fx.say("keep reading");

        let mut script: Vec<_> = (0..LOOP_STREAK)
            .map(|round| {
                tool_call_script(
                    &format!("call_{round}"),
                    tool::FS_READ,
                    r#"{"path":"a.txt"}"#,
                )
            })
            .collect();
        script.push(vec![
            ModelEvent::TextDelta {
                text: "stopped; here is what I have".to_owned(),
            },
            ModelEvent::Finish {
                reason: StopReason::Stop,
                usage: None,
            },
        ]);

        let provider = FakeProvider::scripted(script);
        fx.turn(&provider)
            .run(&fx.plan(), &CancellationToken::new())
            .await;

        let transcript = fx.transcript();
        let last = transcript.last().expect("a final assistant message");
        assert_eq!(last.role, crate::store::Role::Assistant);
        assert!(
            last.text.contains("what I have"),
            "the wrap-up round reached the transcript: {}",
            last.text
        );
    }

    /// A turn that ends mid-run hands the run to the next one, so the
    /// procedure the cap interrupted resumes under its own name
    /// (`IDEAS.md` § 10).
    #[tokio::test]
    async fn a_turn_that_ends_mid_run_carries_it_to_the_next_turn() {
        let mut fx = Fixture::new();
        grant_runbook(&mut fx, "inbox.triage");
        fx.say("triage it");

        let provider = FakeProvider::scripted(vec![tool_call_script(
            "call_open",
            tool::SKILL_RUN,
            r#"{"name":"inbox.triage"}"#,
        )]);
        fx.turn(&provider)
            .run(&fx.plan(), &CancellationToken::new())
            .await;

        assert_eq!(
            fx.turns.open_run(&fx.session_id).as_deref(),
            Some("inbox.triage"),
            "the run outlives the turn that opened it"
        );
    }

    /// Except when the user pressed Stop, which is the clearest statement
    /// there is that the conversation has moved on.
    #[tokio::test]
    async fn a_cancelled_turn_closes_the_run_rather_than_carrying_it() {
        let mut fx = Fixture::new();
        grant_runbook(&mut fx, "inbox.triage");
        fx.say("triage it");

        let provider = FakeProvider::scripted(vec![
            tool_call_script("call_open", tool::SKILL_RUN, r#"{"name":"inbox.triage"}"#),
            tool_call_script(
                "call_write",
                tool::FS_WRITE,
                r#"{"path":"new.txt","content":"x"}"#,
            ),
        ]);

        // Cancelled on the write's dialog, where a person would press Stop.
        let cancel = CancellationToken::new();
        let stopping = async {
            fx.pending().await;
            cancel.cancel();
        };
        let turn = fx.turn(&provider);
        let plan = fx.plan();
        let (reason, ()) = tokio::join!(turn.run(&plan, &cancel), stopping);

        assert_eq!(reason, StopReason::Cancelled);
        assert_eq!(
            fx.turns.open_run(&fx.session_id),
            None,
            "Stop ends the run, not just the turn"
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
                thought_signature: None,
            },
            ModelEvent::ToolCallDelta {
                index: 1,
                id: Some("call_b".to_owned()),
                name: Some(tool::FS_READ.to_owned()),
                args_delta: r#"{"path":"nope.txt"}"#.to_owned(),
                thought_signature: None,
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

    /// A large `fs_write` streams no text; drafting events show the call is
    /// still being written.
    #[tokio::test]
    async fn a_call_being_written_reports_how_far_it_has_got() {
        let fx = Fixture::new();
        fx.say("write the file");

        // Three fragments of one call's arguments, as a real stream sends
        // them: the name arrives once, the rest is content.
        let provider = FakeProvider::scripted(vec![vec![
            ModelEvent::ToolCallDelta {
                index: 0,
                id: Some("call_a".to_owned()),
                name: Some(tool::FS_LIST.to_owned()),
                args_delta: r#"{"pa"#.to_owned(),
                thought_signature: None,
            },
            ModelEvent::ToolCallDelta {
                index: 0,
                id: None,
                name: None,
                args_delta: r#"th":"#.to_owned(),
                thought_signature: None,
            },
            ModelEvent::ToolCallDelta {
                index: 0,
                id: None,
                name: None,
                args_delta: r#""."}"#.to_owned(),
                thought_signature: None,
            },
            ModelEvent::Finish {
                reason: StopReason::ToolCalls,
                usage: None,
            },
        ]]);

        fx.turn(&provider)
            .run(&fx.plan(), &CancellationToken::new())
            .await;

        let drafts: Vec<ToolDrafting> = fx
            .sink
            .events()
            .into_iter()
            .filter_map(|event| match event {
                Event::ToolDrafting(draft) => Some(draft),
                _ => None,
            })
            .collect();

        let last = drafts.last().expect("the call was reported as it arrived");
        assert_eq!(
            last.bytes,
            r#"{"path":"."}"#.len() as u64,
            "the count is the whole arguments string, fragments summed"
        );
        assert_eq!(last.tool.as_deref(), Some(tool::FS_LIST));
        assert_eq!(last.index, 0);
    }

    /// An unchanged drafting size is not re-reported on every frame.
    #[test]
    fn an_unchanged_count_stays_quiet() {
        let fx = Fixture::new();
        let plan = fx.plan();
        let provider = FakeProvider::new();
        let turn = fx.turn(&provider);

        let mut draft = Drafting::new(0);
        draft.tool = Some(tool::FS_WRITE.to_owned());
        draft.bytes = 4_096;
        let mut seq = 0;

        turn.emit_drafting(&plan, &mut draft, &mut seq);
        turn.emit_drafting(&plan, &mut draft, &mut seq);

        assert_eq!(seq, 1, "the second call had nothing new to say");
        assert_eq!(
            fx.sink
                .names()
                .iter()
                .filter(|name| **name == crate::agent::event::name::TOOL_DRAFTING)
                .count(),
            1
        );

        draft.bytes = 8_192;
        turn.emit_drafting(&plan, &mut draft, &mut seq);
        assert_eq!(seq, 2, "a count that moved is reported");
    }

    #[test]
    fn a_finished_turn_leaves_the_session_in_a_drawable_state() {
        assert_eq!(resting_state(StopReason::Stop), SessionState::Idle);
        assert_eq!(resting_state(StopReason::Cancelled), SessionState::Idle);
        assert_eq!(resting_state(StopReason::Length), SessionState::Idle);
        assert_eq!(resting_state(StopReason::Error), SessionState::Error);
    }
}
