//! The session document: `sessions.json`.
//!
//! A session is a conversation inside a project: a title, a transcript, and
//! the timestamps that order it in the sidebar. Everything the UI shows about
//! a session that survives a restart is here; everything that does not — most
//! importantly whether a turn is running — is deliberately absent.
//!
//! That absence is the one design decision worth stating up front. A session's
//! [`SessionState`] is derived on read from the live turn registry, never
//! persisted, for the same reason `workspace_exists` is never persisted: if
//! Aegis is killed mid-turn, a stored `running` would come back after the
//! restart and there would be nothing left to finish it. A session that was
//! interrupted is idle, because that is what it is.
//!
//! One document holds every session of every project. At MVP scale — a handful
//! of projects, transcripts a human typed — that is a file a person can open
//! and read, which is worth more than the write amplification it costs. A
//! transcript that outgrows it wants its own file; the seam for that is
//! [`SessionStore::save`], the only place the whole document is serialized.
//!
//! From Phase 14 a session may also carry a [`Compaction`], and it is worth
//! being clear about what that is not: it is a *pointer* plus derived state,
//! never a deletion. The transcript above it stays in this document, whole and
//! in order — [`SessionStore::compact`] adds a record and removes no message.
//! What a fold changes is which messages [`compact::tail`] hands to the model,
//! and nothing else. A store that trimmed the transcript to save the model
//! tokens would be destroying the only copy of a conversation somebody is
//! still reading.
//!
//! From Phase 17 it also carries what each turn *cost*: one [`TurnCost`] per
//! finished turn, and [`Cost`] over any set of them. That is here rather than
//! on the audit line for a reason worth writing down, because PLAN 7.1 lists
//! `tokens` beside `agent_id` and `skill` as a field the log could grow.
//! **Tokens are not a property of a tool call.** They are spent by a model
//! round, several calls can come out of one round, and — the fact that settles
//! it — a turn that called no tool at all still spends them. A counter built
//! from the audit log would silently omit every reply that only talked, which
//! is most of them. So cost is recorded where it is spent, keyed by the same
//! `turn_id` the audit line carries, and a run's cost is the join of the two
//! ([`board::trace`](crate::board::trace)).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::approval::ApprovalRequest;
use crate::compact::{self, Plan};

use super::agents::DEFAULT_AGENT_ID;
use super::{now, quarantine, strip_bom, write_atomic};
use crate::error::{AppError, AppResult};

/// Name of the document under the application-data directory.
const SESSIONS_FILE: &str = "sessions.json";

/// Schema version of [`SessionsFile`].
///
/// Independent of the project document's version: the two files change at very
/// different rates, and a migration to one has no business quarantining the
/// other.
const SCHEMA_VERSION: u32 = 1;

/// Title given to a session created without one.
const DEFAULT_TITLE: &str = "New session";

/// How much of the first user message becomes the session title.
///
/// Long enough to tell two sessions apart in a sidebar, short enough not to
/// wrap.
const TITLE_MAX_CHARS: usize = 48;

// ---------------------------------------------------------------------------
// IPC payloads (PLAN 2.1, "Sessions and turns")
// ---------------------------------------------------------------------------

/// Lifecycle of a session.
///
/// Derived from the turn registry on every read, never stored — see the module
/// documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum SessionState {
    /// Nothing is running. The composer is open.
    #[default]
    Idle,
    /// A turn is streaming.
    Running,
    /// A turn is blocked on an approval (Phase 6).
    AwaitingApproval,
    /// The last turn ended with an error. Cleared by the next send.
    Error,
}

/// Who produced a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum Role {
    /// Typed by the person.
    User,
    /// Produced by the model.
    Assistant,
    /// A tool result, answering one of the assistant's calls.
    Tool,
    /// The instructions the runtime prepends.
    ///
    /// Never persisted in a transcript: it is rebuilt for every request,
    /// because the workspace path it names can change between turns.
    System,
}

/// How far one tool call got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum ToolCallStatus {
    /// Assembled, not yet judged by policy.
    Pending,
    /// Cleared to run.
    Approved,
    /// Refused, by policy or by the user.
    Denied,
    /// Executing.
    Running,
    /// Finished, and did what was asked.
    Ok,
    /// Finished, and did not.
    Error,
    /// Abandoned because the turn was cancelled.
    Cancelled,
}

/// One tool call, as the transcript records it.
///
/// `args_json` is what the model actually sent, kept verbatim so the card in
/// the UI shows the call that was made rather than the call policy resolved.
/// `summary` is the one human line — never a raw blob, because a transcript
/// that inlines 200 KB of file content is a transcript nobody scrolls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct ToolCallRecord {
    /// The model's own id for the call.
    pub call_id: String,
    /// Tool name: `fs_list`, `fs_read`, `fs_write`, ...
    pub tool: String,
    /// The arguments as the model sent them, as a JSON string.
    pub args_json: String,
    /// How far it got.
    pub status: ToolCallStatus,
    /// One human line naming the result, or `null` before there is one.
    pub summary: Option<String>,
    /// A local image the call produced, for the transcript to show.
    ///
    /// Only `screen_capture` sets it, and it is a path rather than the bytes
    /// (PLAN 5.4): the WebView loads it through the asset protocol, which is
    /// scoped to the capture directory. Persisted, so re-opening a session
    /// shows the capture again instead of a line saying one was taken.
    ///
    /// `#[serde(default)]` for the transcripts written before this field
    /// existed — a session on disk must keep opening.
    #[serde(default)]
    pub image_path: Option<String>,
    /// Gemini thought signature to echo on the next request.
    ///
    /// Opaque encrypted state. Not shown in the WebView (`ts(skip)`); kept on
    /// disk so a tool round after a restart still has it. Missing in
    /// transcripts written before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(skip)]
    pub thought_signature: Option<String>,
}

/// One message in a transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct Message {
    /// UUID v4.
    pub id: String,
    /// Who produced it.
    pub role: Role,
    /// The text. Empty for an assistant message that only made tool calls.
    pub text: String,
    /// The calls this message made. Assistant messages only.
    #[serde(default)]
    pub tool_calls: Vec<ToolCallRecord>,
    /// Which call this message answers. `tool` messages only.
    #[serde(default)]
    pub tool_call_id: Option<String>,
    /// RFC3339, UTC.
    pub created_at: String,
}

impl Message {
    /// A message from the person.
    pub fn user(text: impl Into<String>) -> Self {
        Self::new(Role::User, text.into())
    }

    /// A message from the model, with whatever calls it made.
    pub fn assistant(text: impl Into<String>, tool_calls: Vec<ToolCallRecord>) -> Self {
        Self {
            tool_calls,
            ..Self::new(Role::Assistant, text.into())
        }
    }

    /// A tool result answering `call_id`. The text is a `ToolResult` envelope.
    pub fn tool(call_id: impl Into<String>, envelope: impl Into<String>) -> Self {
        Self {
            tool_call_id: Some(call_id.into()),
            ..Self::new(Role::Tool, envelope.into())
        }
    }

    /// The shared skeleton.
    fn new(role: Role, text: String) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            role,
            text,
            tool_calls: Vec::new(),
            tool_call_id: None,
            created_at: now(),
        }
    }

    /// Whether this message carries nothing at all.
    ///
    /// A turn that produced no text and made no calls has nothing to record,
    /// and an empty bubble in the transcript is worse than no bubble.
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.tool_calls.is_empty()
    }
}

/// One row of a project's session list.
///
/// `state` is live and `message_count` is derived; neither is stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct SessionSummary {
    /// UUID v4.
    pub id: String,
    /// The project this session belongs to.
    pub project_id: String,
    /// The identity this session runs as (PLAN 7.3, Phase 12).
    ///
    /// Always a concrete id, never absent: a session written before identities
    /// existed stored nothing, and resolves to
    /// [`DEFAULT_AGENT_ID`](super::agents::DEFAULT_AGENT_ID) here. The store
    /// keeps the distinction — "chose nothing" and "chose the default" are
    /// different facts about a document — and the payload does not, because a
    /// UI that had to handle both would draw the same badge twice.
    pub agent_id: String,
    /// Display title. Taken from the first user message when not given.
    pub title: String,
    /// RFC3339, UTC.
    pub created_at: String,
    /// RFC3339, UTC. Bumped by every message and every rename.
    pub updated_at: String,
    /// How many messages the transcript holds.
    pub message_count: u32,
    /// What the session is doing *right now*.
    pub state: SessionState,
    /// The brief that opened this session, when one did (PLAN 7.3, Phase 15).
    ///
    /// `None` for every session a person started, which is every session before
    /// this phase. The sidebar draws it as a badge rather than hiding the row:
    /// work done on your behalf should be as visible as work you asked for.
    pub delegated: Option<Delegated>,
    /// The routine that opened this session, when one did (PLAN 7.3, Phase 16).
    ///
    /// The other way a session comes to exist without anybody typing. It is a
    /// second field rather than a variant beside [`SessionSummary::delegated`]
    /// because the two are different facts and a session could one day be both
    /// — a routine's run is not a brief, and a brief is not on a clock.
    pub scheduled: Option<Scheduled>,
    /// What the whole conversation has spent (PLAN 7.3, Phase 17).
    ///
    /// Summed on read from the per-turn records rather than kept as a running
    /// total, for the reason `message_count` is: a stored aggregate is a second
    /// copy of a fact, and the two disagree the first time anything goes wrong.
    pub cost: Cost,
}

/// A session and its transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct SessionDetail {
    /// The session itself.
    pub session: SessionSummary,
    /// Oldest first — the order the transcript is read in.
    pub messages: Vec<Message>,
    /// What has been folded out of the model's context, if anything.
    ///
    /// On the detail rather than on the summary because it is about the
    /// *transcript*, and the summary is a sidebar row: what a fold changes is
    /// how the conversation is drawn, and the sidebar does not draw one.
    pub compaction: Option<Compaction>,
    /// Approvals this session is blocked on.
    ///
    /// Filled by [`AppState::session_detail`](crate::AppState::session_detail)
    /// rather than here: the transcript is on disk, and what a session is
    /// waiting for is a fact about this process. It is on the detail at all so
    /// that a window reopened mid-turn re-draws the dialog it missed, instead
    /// of leaving a turn blocked on a prompt nobody can see.
    pub pending_approvals: Vec<ApprovalRequest>,
}

/// Why a session exists, when a person did not open it (PLAN 7.3, Phase 15).
///
/// A delegated run is an ordinary session in every way that matters — same
/// transcript, same approval gate, same audit lines, same identity binding —
/// and this record is the difference: it says which delegation opened it, which
/// session was delegating, and where the brief was filed.
///
/// It is on the session rather than in a store of its own because a delegated
/// run *is* a session, and a second document listing which sessions are really
/// runs would be a second thing to keep in step with this one. It is also what
/// keeps the work visible: a specialist's session opens in the sidebar like any
/// other, so "what did the reviewer actually do" is a click rather than a
/// forensic exercise (`COS.md` aggregates status for the *CoS*, not for the
/// person).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct Delegated {
    /// The delegation this run belongs to. Shared with the audit lines.
    pub handoff_id: String,
    /// The session whose turn handed the brief out.
    pub from_session_id: String,
    /// Where the brief was filed, relative to the workspace root.
    ///
    /// `None` when the workspace has no `.aegis/briefs/` — the brief then lives only
    /// in the first message of this transcript, which is still a record of it.
    pub brief: Option<String>,
}

/// Why a session exists, when a clock opened it (PLAN 7.3, Phase 16).
///
/// The routine's record on the run, and the mirror of [`Delegated`]: a
/// scheduled run is an ordinary session in every way that matters, and this is
/// the difference. The routine's *name* is copied rather than only its id, so a
/// row still says what fired it after the routine has been renamed or deleted —
/// a transcript is a record of something that happened, and it should not stop
/// explaining itself because a document moved on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct Scheduled {
    /// The routine that fired it. Shared with the audit lines the run writes.
    pub routine_id: String,
    /// Its name when it fired.
    pub routine_name: String,
    /// The runbook it was fired to run.
    pub skill: String,
}

/// What a session has folded, and what it folded to (PLAN 7.3, Phase 14).
///
/// A pointer and a summary, never a deletion: `through_message_id` names the
/// last message that no longer reaches the model, and the transcript on disk
/// still holds every one of them. That split is the whole design — the user
/// keeps scrolling through the conversation they had, and the model stops
/// paying for it ([`compact`](crate::compact)).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct Compaction {
    /// The last message that folds. Everything after it still reaches the
    /// model.
    pub through_message_id: String,
    /// How many messages folded.
    pub folded: u32,
    /// What they became: goal, files, decisions, blockers.
    pub state: String,
    /// RFC3339, UTC.
    pub at: String,
}

/// What one turn spent, as the provider reported it (PLAN 7.3, Phase 17).
///
/// One record per finished turn, whatever the turn did — a reply that only
/// talked costs tokens as surely as one that ran six tools, and a ledger that
/// only counted the second kind would answer "what did it cost" with a number
/// nobody could reconcile against a bill.
///
/// `turn_id` is the same id the audit line carries, which is the whole reason
/// this can be joined to a run: the log says *which* turns a delegation or a
/// runbook made its calls in, and this says what each of those turns spent.
///
/// `reported` is the honesty flag. Not every OpenAI-compatible server sends a
/// `usage` object, and a turn whose cost is unknown is recorded as unknown
/// rather than as zero — a counter that quietly added nothing would read as a
/// free turn, which is the one thing it certainly was not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct TurnCost {
    /// The turn this is the cost of.
    pub turn_id: String,
    /// Tokens in the requests this turn made, cached ones included.
    #[ts(type = "number")]
    pub prompt_tokens: u64,
    /// Of those, how many were served out of the prompt cache.
    ///
    /// Defaulted rather than required: sessions charged before this was
    /// recorded are read back as turns that cached nothing, which is what a
    /// turn that predates the field did.
    #[serde(default)]
    #[ts(type = "number")]
    pub cache_read_tokens: u64,
    /// Of those, how many were written to the cache.
    #[serde(default)]
    #[ts(type = "number")]
    pub cache_creation_tokens: u64,
    /// Tokens in the replies it got back.
    #[ts(type = "number")]
    pub completion_tokens: u64,
    /// Whether the provider actually said. `false` means the counts above
    /// are zero because nothing was reported, not because nothing was spent.
    pub reported: bool,
    /// RFC3339, UTC. When the turn finished.
    pub at: String,
}

impl TurnCost {
    /// A turn whose provider reported what it spent.
    ///
    /// The two counts are per turn, not per request: a turn that ran three
    /// rounds of tool calls made three requests, and what is stored is their
    /// sum, because the turn is the smallest thing a person asked for.
    pub fn reported(turn_id: &str, prompt_tokens: u64, completion_tokens: u64) -> Self {
        Self {
            turn_id: turn_id.to_owned(),
            prompt_tokens,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            completion_tokens,
            reported: true,
            at: now(),
        }
    }

    /// How much of [`Self::prompt_tokens`] went through the cache.
    ///
    /// Separate from `reported` because it is a different kind of silence: a
    /// provider that reports usage but no cache figures is one that does not
    /// cache, and zero is the honest answer for it — unlike a provider that
    /// reported nothing at all, which is what `reported: false` is for.
    pub const fn with_cache(mut self, read: u64, created: u64) -> Self {
        self.cache_read_tokens = read;
        self.cache_creation_tokens = created;
        self
    }

    /// A turn whose provider said nothing about what it spent.
    pub fn unreported(turn_id: &str) -> Self {
        Self {
            turn_id: turn_id.to_owned(),
            prompt_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            completion_tokens: 0,
            reported: false,
            at: now(),
        }
    }

    /// Prompt plus completion.
    pub const fn total(&self) -> u64 {
        self.prompt_tokens + self.completion_tokens
    }
}

/// Tokens spent over some set of turns (PLAN 7.3, Phase 17).
///
/// The unit the UI counts in. `unreported` is carried beside the totals rather
/// than folded into them so a number can say how much of itself is missing: a
/// session of ten turns where three providers stayed silent is *at least* this
/// many tokens, and a board that could not say "at least" would be inventing
/// precision it does not have.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct Cost {
    /// How many turns were summed.
    pub turns: u32,
    /// Of those, how many reported nothing.
    pub unreported: u32,
    /// Tokens into the model, cached ones included.
    #[ts(type = "number")]
    pub prompt_tokens: u64,
    /// Of those, how many were served out of the prompt cache. The share of
    /// `prompt_tokens` this is, is how well the prompt is holding its shape.
    #[serde(default)]
    #[ts(type = "number")]
    pub cache_read_tokens: u64,
    /// Of those, how many were written to it.
    #[serde(default)]
    #[ts(type = "number")]
    pub cache_creation_tokens: u64,
    /// Tokens out of it.
    #[ts(type = "number")]
    pub completion_tokens: u64,
}

impl Cost {
    /// Sums the turns an iterator yields.
    pub fn of<'a>(turns: impl IntoIterator<Item = &'a TurnCost>) -> Self {
        turns.into_iter().fold(Self::default(), |mut cost, turn| {
            cost.turns = cost.turns.saturating_add(1);
            if turn.reported {
                cost.prompt_tokens = cost.prompt_tokens.saturating_add(turn.prompt_tokens);
                cost.cache_read_tokens = cost
                    .cache_read_tokens
                    .saturating_add(turn.cache_read_tokens);
                cost.cache_creation_tokens = cost
                    .cache_creation_tokens
                    .saturating_add(turn.cache_creation_tokens);
                cost.completion_tokens = cost
                    .completion_tokens
                    .saturating_add(turn.completion_tokens);
            } else {
                cost.unreported = cost.unreported.saturating_add(1);
            }
            cost
        })
    }

    /// Prompt plus completion.
    pub const fn total(&self) -> u64 {
        self.prompt_tokens + self.completion_tokens
    }

    /// Adds another sum into this one.
    pub fn add(&mut self, other: Self) {
        self.turns = self.turns.saturating_add(other.turns);
        self.unreported = self.unreported.saturating_add(other.unreported);
        self.prompt_tokens = self.prompt_tokens.saturating_add(other.prompt_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(other.cache_read_tokens);
        self.cache_creation_tokens = self
            .cache_creation_tokens
            .saturating_add(other.cache_creation_tokens);
        self.completion_tokens = self
            .completion_tokens
            .saturating_add(other.completion_tokens);
    }

    /// Whether anything at all is known here.
    ///
    /// A board draws nothing rather than "0 tokens" for a run that never
    /// reached a model.
    pub const fn is_empty(&self) -> bool {
        self.turns == 0
    }
}

/// What `session_send` hands back (PLAN 2.1).
///
/// The turn itself is reported through events; this is only the handle needed
/// to cancel it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct TurnHandle {
    /// The session the turn runs in.
    pub session_id: String,
    /// The turn, unique within the process.
    pub turn_id: String,
}

// ---------------------------------------------------------------------------
// On-disk shapes
// ---------------------------------------------------------------------------

/// The document itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionsFile {
    version: u32,
    sessions: Vec<StoredSession>,
}

/// A session record as persisted.
///
/// Deliberately not [`SessionSummary`]: `state` and `message_count` are both
/// derived, and keeping the two types apart makes it impossible to persist
/// either by accident.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredSession {
    id: String,
    project_id: String,
    /// The identity this session is bound to.
    ///
    /// `#[serde(default)]` and `Option` together are the migration: every
    /// session written before Phase 12 has no such key, reads back as `None`,
    /// and resolves to the built-in identity — which is the assistant it was
    /// already running as. Nothing is rewritten on load.
    #[serde(default)]
    agent_id: Option<String>,
    title: String,
    created_at: String,
    updated_at: String,
    #[serde(default)]
    messages: Vec<Message>,
    /// What has been folded, or `None` for a session that never has been.
    ///
    /// `#[serde(default)]` is the migration, as it was for `agent_id`: a
    /// session written before Phase 14 reads back with none, which is exactly
    /// what it had — a transcript that reaches the model whole.
    #[serde(default)]
    compaction: Option<Compaction>,
    /// The brief that opened this session, when one did.
    ///
    /// `#[serde(default)]` is the migration, as it was for `agent_id` and
    /// `compaction`: a session written before Phase 15 reads back with none,
    /// which is exactly what it is — one somebody opened themselves.
    #[serde(default)]
    delegated: Option<Delegated>,
    /// The routine that opened this session, when one did.
    ///
    /// `#[serde(default)]` is the migration, for the fourth time and for the
    /// same reason: a session written before Phase 16 reads back with none,
    /// which is exactly what it is — one nothing scheduled.
    #[serde(default)]
    scheduled: Option<Scheduled>,
    /// What each finished turn spent, oldest first.
    ///
    /// A list rather than a running total because a *run* is rarely a whole
    /// session: a runbook, or a brief, occupies some of a conversation's turns
    /// and not others, and only the per-turn rows can answer what that part of
    /// it cost. The audit line names the turn; this says what the turn spent.
    ///
    /// `#[serde(default)]` is the migration, for the fifth time: a session
    /// written before Phase 17 reads back with none, which is what is known
    /// about it — nothing was recorded, so nothing is claimed.
    #[serde(default)]
    costs: Vec<TurnCost>,
}

impl StoredSession {
    /// The row the sidebar draws, at the state the caller measured.
    fn to_summary(&self, state: SessionState) -> SessionSummary {
        SessionSummary {
            id: self.id.clone(),
            project_id: self.project_id.clone(),
            agent_id: self
                .agent_id
                .clone()
                .unwrap_or_else(|| DEFAULT_AGENT_ID.to_owned()),
            title: self.title.clone(),
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
            // A transcript that overflows `u32` is not a transcript.
            message_count: u32::try_from(self.messages.len()).unwrap_or(u32::MAX),
            state,
            delegated: self.delegated.clone(),
            scheduled: self.scheduled.clone(),
            cost: Cost::of(&self.costs),
        }
    }
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// The session store: the in-memory sessions plus the document backing them.
///
/// Same shape as [`Store`](super::projects::Store), and for the same reasons:
/// one mutex over the whole list, written out on every mutation, so "what is
/// on disk" always equals "what is in memory" once a call returns.
///
/// Every method that produces a [`SessionSummary`] is *given* the state to
/// stamp on it rather than inventing one. The store cannot see the turn
/// registry, and a default of `Idle` invented here is exactly the lie that
/// would draw a running session as idle.
#[derive(Debug)]
pub struct SessionStore {
    path: PathBuf,
    sessions: Mutex<Vec<StoredSession>>,
}

impl SessionStore {
    /// Loads the store from `data_dir`.
    ///
    /// Never fails, for the reason [`Store::load`](super::projects::Store::load)
    /// does not: a tray app that will not boot cannot explain why it did not.
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(SESSIONS_FILE);

        let sessions = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<SessionsFile>(strip_bom(&bytes)) {
                Ok(file) if file.version == SCHEMA_VERSION => {
                    tracing::info!(count = file.sessions.len(), "session store loaded");
                    file.sessions
                }
                Ok(file) => {
                    tracing::error!(
                        found = file.version,
                        expected = SCHEMA_VERSION,
                        "unknown session store version"
                    );
                    quarantine(&path);
                    Vec::new()
                }
                Err(err) => {
                    tracing::error!(%err, "session store is not readable JSON");
                    quarantine(&path);
                    Vec::new()
                }
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                tracing::info!("no session store yet; starting empty");
                Vec::new()
            }
            Err(err) => {
                tracing::error!(%err, "could not read the session store");
                Vec::new()
            }
        };

        Self {
            path,
            sessions: Mutex::new(sessions),
        }
    }

    /// Locks the list, recovering from a poisoned mutex.
    ///
    /// Same reasoning as the project store: the guarded value is a `Vec` that
    /// is only ever replaced wholesale, so it cannot be torn, and propagating
    /// a panic through every later command is strictly worse.
    fn sessions(&self) -> MutexGuard<'_, Vec<StoredSession>> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Creates a session in `project_id`, bound to `agent_id`.
    ///
    /// An empty title becomes [`DEFAULT_TITLE`], which the first user message
    /// then replaces — see [`SessionStore::append`].
    ///
    /// The identity is fixed here and never changed afterwards. There is
    /// deliberately no way to rebind one: a transcript is a record of what an
    /// identity did, and moving it under a different one would leave `fs_write`
    /// calls in the history of an identity that was never allowed to make any.
    /// Working as someone else is a new session, which costs a click.
    ///
    /// Whether the identity exists is checked by the caller — this store cannot
    /// see the agent document, and
    /// [`AppState::create_session`](crate::AppState::create_session) can.
    pub fn create(
        &self,
        project_id: &str,
        title: Option<&str>,
        agent_id: &str,
    ) -> AppResult<SessionSummary> {
        self.open_session(project_id, title, agent_id, None, None)
    }

    /// Creates the session a brief opens (PLAN 7.3, Phase 15).
    ///
    /// The same call with a [`Delegated`] on it. Deliberately the same session
    /// in every other respect: a specialist's run is gated, audited, titled and
    /// listed exactly as a session someone typed into, because it is one.
    pub fn create_delegated(
        &self,
        project_id: &str,
        title: Option<&str>,
        agent_id: &str,
        delegated: Delegated,
    ) -> AppResult<SessionSummary> {
        self.open_session(project_id, title, agent_id, Some(delegated), None)
    }

    /// Creates the session a routine opens (PLAN 7.3, Phase 16).
    ///
    /// The same call again, with a [`Scheduled`] on it, and for the same
    /// reason: a run nobody watched should be as readable afterwards as one
    /// somebody sat through.
    pub fn create_scheduled(
        &self,
        project_id: &str,
        title: Option<&str>,
        agent_id: &str,
        scheduled: Scheduled,
    ) -> AppResult<SessionSummary> {
        self.open_session(project_id, title, agent_id, None, Some(scheduled))
    }

    /// The one place a session record is built.
    fn open_session(
        &self,
        project_id: &str,
        title: Option<&str>,
        agent_id: &str,
        delegated: Option<Delegated>,
        scheduled: Option<Scheduled>,
    ) -> AppResult<SessionSummary> {
        let stamp = now();
        let session = StoredSession {
            id: Uuid::new_v4().to_string(),
            project_id: project_id.to_owned(),
            agent_id: Some(agent_id.to_owned()),
            title: match title.map(str::trim) {
                Some("") | None => DEFAULT_TITLE.to_owned(),
                Some(given) => given.to_owned(),
            },
            created_at: stamp.clone(),
            updated_at: stamp,
            messages: Vec::new(),
            compaction: None,
            delegated,
            scheduled,
            costs: Vec::new(),
        };
        let created = session.to_summary(SessionState::Idle);

        let mut sessions = self.sessions();
        sessions.push(session);
        self.save(&sessions)?;

        tracing::info!(id = %created.id, project_id, agent_id, "session created");
        Ok(created)
    }

    /// A project's sessions, most recently active first.
    ///
    /// `state_of` is asked for each session's live state; the store has no way
    /// to know it. Timestamps are fixed-width UTC RFC3339, so comparing them as
    /// strings *is* comparing them as instants.
    pub fn list(
        &self,
        project_id: &str,
        state_of: &dyn Fn(&str) -> SessionState,
    ) -> Vec<SessionSummary> {
        let sessions = self.sessions();

        let mut out: Vec<SessionSummary> = sessions
            .iter()
            .filter(|s| s.project_id == project_id)
            .map(|s| s.to_summary(state_of(&s.id)))
            .collect();
        out.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| b.created_at.cmp(&a.created_at))
        });
        out
    }

    /// One session with its transcript.
    pub fn open(&self, id: &str, state: SessionState) -> AppResult<SessionDetail> {
        let sessions = self.sessions();
        let session = Self::find(&sessions, id)?;

        Ok(SessionDetail {
            session: session.to_summary(state),
            messages: session.messages.clone(),
            compaction: session.compaction.clone(),
            // Composed in by `AppState`, which can see the approval registry.
            pending_approvals: Vec::new(),
        })
    }

    /// The session as one row, without its transcript.
    pub fn summary(&self, id: &str, state: SessionState) -> AppResult<SessionSummary> {
        let sessions = self.sessions();
        Ok(Self::find(&sessions, id)?.to_summary(state))
    }

    /// The transcript alone, for building a model request.
    pub fn messages(&self, id: &str) -> AppResult<Vec<Message>> {
        let sessions = self.sessions();
        Ok(Self::find(&sessions, id)?.messages.clone())
    }

    /// The transcript and what has been folded out of it (PLAN 7.3, Phase 14).
    ///
    /// Both under one lock, because they are one fact. Read separately, a
    /// compaction landing between the two reads would produce a pointer into a
    /// transcript that does not match it, and the request built from the pair
    /// would be missing a turn nobody folded.
    pub fn context(&self, id: &str) -> AppResult<(Vec<Message>, Option<Compaction>)> {
        let sessions = self.sessions();
        let session = Self::find(&sessions, id)?;
        Ok((session.messages.clone(), session.compaction.clone()))
    }

    /// Folds the older part of a transcript into state, and reports what is
    /// now folded.
    ///
    /// `force` is the button; without it the transcript also has to have grown
    /// past [`compact::COMPACT_AT_BYTES`]. `Ok(None)` means nothing moved —
    /// too few turns, or a fold that would land exactly where the last one did
    /// — which is an ordinary answer and not a failure: pressing "compact" on
    /// a short session should say "there is nothing to fold", not fail.
    ///
    /// The state is re-derived from the messages every time rather than folded
    /// into whatever the previous compaction said. Deriving from the record is
    /// what keeps a session compacted five times from being a summary of a
    /// summary of a summary — the transcript is still all there, so there is
    /// never a reason to compound.
    ///
    /// `updated_at` is deliberately not touched. A fold is maintenance, not
    /// activity, and a sidebar that reordered itself because a session tidied
    /// its own context would be reporting something that did not happen.
    pub fn compact(&self, id: &str, force: bool) -> AppResult<Option<Compaction>> {
        let mut sessions = self.sessions();
        let session = Self::find_mut(&mut sessions, id)?;

        let Some(Plan {
            through_message_id,
            folded,
            state,
        }) = compact::plan(&session.messages, force)
        else {
            return Ok(None);
        };

        if session
            .compaction
            .as_ref()
            .is_some_and(|held| held.through_message_id == through_message_id)
        {
            tracing::debug!(session_id = id, "nothing new to fold");
            return Ok(None);
        }

        let compaction = Compaction {
            through_message_id,
            folded,
            state,
            at: now(),
        };
        session.compaction = Some(compaction.clone());

        self.save(&sessions)?;
        tracing::info!(session_id = id, folded, force, "session compacted");
        Ok(Some(compaction))
    }

    /// Records what a turn spent (PLAN 7.3, Phase 17).
    ///
    /// Called once per finished turn, whatever the turn did and however it
    /// ended — a cancelled turn spent the tokens it had already spent, and a
    /// ledger that only counted the tidy endings would be one nobody could
    /// reconcile.
    ///
    /// `updated_at` is deliberately not touched, for the reason
    /// [`SessionStore::compact`] does not touch it: the turn that just ran has
    /// already bumped it by appending its message, and stamping it a second
    /// time would reorder the sidebar for a bookkeeping write.
    ///
    /// Charging the same turn twice replaces rather than adds. Nothing calls it
    /// twice today; if something ever does, the second figure is a correction
    /// of the first, never a second turn's worth of tokens.
    pub fn charge(&self, id: &str, cost: TurnCost) -> AppResult<Cost> {
        let mut sessions = self.sessions();
        let session = Self::find_mut(&mut sessions, id)?;

        match session
            .costs
            .iter_mut()
            .find(|held| held.turn_id == cost.turn_id)
        {
            Some(held) => *held = cost,
            None => session.costs.push(cost),
        }
        let total = Cost::of(&session.costs);

        self.save(&sessions)?;
        Ok(total)
    }

    /// What each of a session's turns spent, oldest first.
    ///
    /// The join a trace makes: given the turn ids on a run's audit lines, this
    /// is what those turns cost ([`board::trace`](crate::board::trace)).
    pub fn costs(&self, id: &str) -> AppResult<Vec<TurnCost>> {
        let sessions = self.sessions();
        Ok(Self::find(&sessions, id)?.costs.clone())
    }

    /// Which project a session belongs to.
    pub fn project_of(&self, id: &str) -> AppResult<String> {
        let sessions = self.sessions();
        Ok(Self::find(&sessions, id)?.project_id.clone())
    }

    /// Which identity a session is bound to, as it was stored.
    ///
    /// `None` is a session written before identities existed, which is not the
    /// same fact as one that chose the default and is deliberately not
    /// flattened into it here — resolving that is
    /// [`AgentStore::resolve`](super::agents::AgentStore::resolve)'s job.
    pub fn agent_of(&self, id: &str) -> AppResult<Option<String>> {
        let sessions = self.sessions();
        Ok(Self::find(&sessions, id)?.agent_id.clone())
    }

    /// How many sessions run as `agent_id`.
    ///
    /// Asked before an identity is deleted. Sessions that named nothing are not
    /// counted against the built-in identity, and do not need to be: the
    /// built-in one cannot be deleted at all.
    pub fn count_for_agent(&self, agent_id: &str) -> usize {
        let sessions = self.sessions();
        sessions
            .iter()
            .filter(|session| session.agent_id.as_deref() == Some(agent_id))
            .count()
    }

    /// Renames a session. An empty title is refused rather than stored.
    pub fn rename(&self, id: &str, title: &str, state: SessionState) -> AppResult<SessionSummary> {
        let title = title.trim();
        if title.is_empty() {
            return Err(AppError::SessionTitle);
        }

        let mut sessions = self.sessions();
        let session = Self::find_mut(&mut sessions, id)?;
        session.title = title.to_owned();
        session.updated_at = now();
        let renamed = session.to_summary(state);

        self.save(&sessions)?;
        tracing::debug!(id, "session renamed");
        Ok(renamed)
    }

    /// Appends a message, and returns the session as it now stands.
    ///
    /// The first user message also names the session, when nothing else has:
    /// a sidebar of six rows all reading "New session" cannot be navigated,
    /// and asking a user to name a conversation before they have had it asks
    /// too early.
    pub fn append(
        &self,
        id: &str,
        message: Message,
        state: SessionState,
    ) -> AppResult<SessionSummary> {
        let mut sessions = self.sessions();
        let session = Self::find_mut(&mut sessions, id)?;

        if session.title == DEFAULT_TITLE && message.role == Role::User {
            session.title = title_from(&message.text);
        }
        session.messages.push(message);
        session.updated_at = now();
        let updated = session.to_summary(state);

        self.save(&sessions)?;
        Ok(updated)
    }

    /// Updates one tool call inside whichever assistant message holds it.
    ///
    /// Addressed by `call_id` alone: the id is the model's, unique within the
    /// turn, and scanning for it costs nothing at transcript scale. `summary`
    /// of `None` leaves the existing one alone, so a status change does not
    /// erase the line a finished call already wrote.
    ///
    /// Returns whether a call was found. A caller that silently updated
    /// nothing is a bug worth seeing in the log.
    pub fn set_tool_call_status(
        &self,
        id: &str,
        call_id: &str,
        status: ToolCallStatus,
        summary: Option<String>,
        image_path: Option<String>,
    ) -> AppResult<bool> {
        let mut sessions = self.sessions();
        let session = Self::find_mut(&mut sessions, id)?;

        let found = session
            .messages
            .iter_mut()
            .flat_map(|message| message.tool_calls.iter_mut())
            .find(|call| call.call_id == call_id)
            .map(|call| {
                call.status = status;
                if summary.is_some() {
                    call.summary = summary;
                }
                // Same rule as the summary, for the same reason: a later
                // status change carrying nothing must not erase what an
                // earlier one recorded.
                if image_path.is_some() {
                    call.image_path = image_path;
                }
            })
            .is_some();

        if found {
            self.save(&sessions)?;
        } else {
            tracing::warn!(
                session_id = id,
                call_id,
                "no such tool call in the transcript"
            );
        }
        Ok(found)
    }

    /// Deletes a session and its transcript.
    pub fn delete(&self, id: &str) -> AppResult<()> {
        let mut sessions = self.sessions();

        let before = sessions.len();
        sessions.retain(|s| s.id != id);
        if sessions.len() == before {
            return Err(AppError::SessionNotFound { id: id.to_owned() });
        }

        self.save(&sessions)?;
        tracing::info!(id, "session deleted");
        Ok(())
    }

    /// Deletes every session of a project, and reports how many went.
    ///
    /// Called when a project is forgotten. Sessions of a project that no
    /// longer exists are unreachable, and leaving them behind would grow the
    /// document forever with transcripts nothing can open.
    pub fn delete_for_project(&self, project_id: &str) -> AppResult<usize> {
        let mut sessions = self.sessions();

        let before = sessions.len();
        sessions.retain(|s| s.project_id != project_id);
        let removed = before - sessions.len();

        if removed > 0 {
            self.save(&sessions)?;
            tracing::info!(project_id, removed, "sessions deleted with their project");
        }
        Ok(removed)
    }

    /// Looks a session up, or reports that the caller's list is stale.
    fn find<'a>(sessions: &'a [StoredSession], id: &str) -> AppResult<&'a StoredSession> {
        sessions
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| AppError::SessionNotFound { id: id.to_owned() })
    }

    /// [`SessionStore::find`], mutably.
    fn find_mut<'a>(
        sessions: &'a mut [StoredSession],
        id: &str,
    ) -> AppResult<&'a mut StoredSession> {
        sessions
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or_else(|| AppError::SessionNotFound { id: id.to_owned() })
    }

    /// Serializes the list and replaces the document atomically.
    ///
    /// Takes the guard, so the only way to reach it is to already hold the
    /// lock: a caller cannot mutate the list and forget to persist it.
    fn save(&self, sessions: &[StoredSession]) -> AppResult<()> {
        let file = SessionsFile {
            version: SCHEMA_VERSION,
            sessions: sessions.to_vec(),
        };

        let bytes = serde_json::to_vec_pretty(&file).map_err(|err| {
            tracing::error!(%err, "could not serialize the session store");
            AppError::Store {
                action: "serialize",
                source: io::Error::other(err),
            }
        })?;

        write_atomic(&self.path, &bytes).map_err(|err| {
            tracing::error!(%err, path = %self.path.display(), "could not save the session store");
            AppError::Store {
                action: "save",
                source: err,
            }
        })
    }
}

/// A session title taken from the first thing the user said.
///
/// Whitespace collapses, so a pasted block does not become a title with a line
/// break in it. The cut prefers the last word boundary in the final quarter of
/// the budget: cutting mid-word reads as a bug, cutting a little short does
/// not.
fn title_from(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return DEFAULT_TITLE.to_owned();
    }

    // `char_indices` rather than byte slicing: a title is user text, and
    // cutting a multi-byte character in half would panic.
    let Some(end) = flat.char_indices().map(|(i, _)| i).nth(TITLE_MAX_CHARS) else {
        return flat;
    };

    let head = &flat[..end];
    let keep = head
        .rfind(' ')
        .filter(|space| space * 4 >= end * 3)
        .unwrap_or(end);

    format!("{}…", head[..keep].trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    /// Every session idle — the state a store built in a test is in.
    fn idle(_id: &str) -> SessionState {
        SessionState::Idle
    }

    /// A store plus the directory it lives in.
    struct Fixture {
        _dir: TempDir,
        data: PathBuf,
        store: SessionStore,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = TempDir::new().expect("temp dir");
            let data = dir.path().join("data");
            fs::create_dir_all(&data).expect("data dir");
            let store = SessionStore::load(&data);
            Self {
                _dir: dir,
                data,
                store,
            }
        }

        fn document(&self) -> PathBuf {
            self.data.join(SESSIONS_FILE)
        }

        /// Reloads from disk, as a restart would.
        fn reopen(&self) -> SessionStore {
            SessionStore::load(&self.data)
        }
    }

    #[test]
    fn what_a_turn_spent_survives_a_restart_and_sums_on_the_row() {
        let fx = Fixture::new();
        let created = fx
            .store
            .create("p1", Some("Refactor"), DEFAULT_AGENT_ID)
            .expect("create");

        fx.store
            .charge(&created.id, TurnCost::reported("t1", 900, 120))
            .expect("charge");
        fx.store
            .charge(&created.id, TurnCost::reported("t2", 1_100, 80))
            .expect("charge");

        let reopened = fx.reopen();
        let row = reopened
            .summary(&created.id, SessionState::Idle)
            .expect("row");
        assert_eq!(row.cost.turns, 2);
        assert_eq!(row.cost.prompt_tokens, 2_000);
        assert_eq!(row.cost.completion_tokens, 200);
        assert_eq!(row.cost.total(), 2_200);
        assert_eq!(row.cost.unreported, 0);

        let turns = reopened.costs(&created.id).expect("the per-turn rows");
        assert_eq!(turns.len(), 2, "the join a trace makes is on the turn");
        assert_eq!(turns[0].turn_id, "t1", "oldest first");
    }

    #[test]
    fn the_cached_share_survives_a_restart_and_sums_with_the_rest() {
        let fx = Fixture::new();
        let created = fx
            .store
            .create("p1", Some("Read the PDFs"), DEFAULT_AGENT_ID)
            .expect("create");

        fx.store
            .charge(
                &created.id,
                TurnCost::reported("t1", 20_000, 400).with_cache(0, 19_000),
            )
            .expect("charge");
        // The second turn is what caching is for: a bigger prompt that cost
        // less, because almost all of it was the first turn's cache entry.
        fx.store
            .charge(
                &created.id,
                TurnCost::reported("t2", 24_000, 300).with_cache(19_000, 4_500),
            )
            .expect("charge");

        let reopened = fx.reopen();
        let row = reopened
            .summary(&created.id, SessionState::Idle)
            .expect("row");
        assert_eq!(row.cost.prompt_tokens, 44_000);
        assert_eq!(row.cost.cache_read_tokens, 19_000);
        assert_eq!(row.cost.cache_creation_tokens, 23_500);
    }

    #[test]
    fn a_turn_charged_before_the_cache_was_counted_still_reads_back() {
        // The two fields are `serde(default)`, so a session file written by an
        // earlier build is a session whose turns cached nothing — not a store
        // that refuses to load.
        let earlier = serde_json::json!({
            "turn_id": "t1",
            "prompt_tokens": 900,
            "completion_tokens": 120,
            "reported": true,
            "at": "2026-09-02T10:00:00Z",
        });

        let cost: TurnCost = serde_json::from_value(earlier).expect("an earlier turn still loads");
        assert_eq!(cost.prompt_tokens, 900);
        assert_eq!(cost.cache_read_tokens, 0);
        assert_eq!(cost.cache_creation_tokens, 0);
        assert!(cost.reported);
    }

    #[test]
    fn a_turn_whose_provider_said_nothing_is_unknown_rather_than_free() {
        let fx = Fixture::new();
        let created = fx
            .store
            .create("p1", None, DEFAULT_AGENT_ID)
            .expect("create");

        fx.store
            .charge(&created.id, TurnCost::reported("t1", 10, 5))
            .expect("charge");
        let total = fx
            .store
            .charge(&created.id, TurnCost::unreported("t2"))
            .expect("charge");

        assert_eq!(total.turns, 2);
        assert_eq!(total.unreported, 1);
        assert_eq!(
            total.total(),
            15,
            "at least this many tokens were spent, and the row says how much of \
             itself is missing"
        );
    }

    #[test]
    fn charging_a_turn_twice_corrects_it_rather_than_doubling_it() {
        let fx = Fixture::new();
        let created = fx
            .store
            .create("p1", None, DEFAULT_AGENT_ID)
            .expect("create");

        fx.store
            .charge(&created.id, TurnCost::reported("t1", 10, 5))
            .expect("charge");
        let total = fx
            .store
            .charge(&created.id, TurnCost::reported("t1", 20, 5))
            .expect("charge again");

        assert_eq!(total.turns, 1);
        assert_eq!(total.total(), 25);
    }

    #[test]
    fn a_cost_does_not_reorder_the_sidebar() {
        let fx = Fixture::new();
        let created = fx
            .store
            .create("p1", Some("Refactor"), DEFAULT_AGENT_ID)
            .expect("create");
        let before = created.updated_at.clone();

        fx.store
            .charge(&created.id, TurnCost::reported("t1", 10, 5))
            .expect("charge");

        let row = fx
            .store
            .summary(&created.id, SessionState::Idle)
            .expect("row");
        assert_eq!(
            row.updated_at, before,
            "bookkeeping is not activity; the turn already bumped this"
        );
    }

    #[test]
    fn a_session_written_before_costs_existed_reads_back_with_none() {
        let fx = Fixture::new();
        let created = fx
            .store
            .create("p1", None, DEFAULT_AGENT_ID)
            .expect("create");

        // The document as an earlier build wrote it: no `costs` key at all.
        let mut document: serde_json::Value =
            serde_json::from_slice(&fs::read(fx.document()).expect("read")).expect("parse");
        document["sessions"][0]
            .as_object_mut()
            .expect("a session object")
            .remove("costs");
        fs::write(
            fx.document(),
            serde_json::to_vec_pretty(&document).expect("serialize"),
        )
        .expect("write");

        let row = fx
            .reopen()
            .summary(&created.id, SessionState::Idle)
            .expect("the older document still loads");
        assert!(
            row.cost.is_empty(),
            "nothing was recorded, so nothing is claimed"
        );
    }

    #[test]
    fn a_session_survives_a_restart() {
        let fx = Fixture::new();
        let created = fx
            .store
            .create("p1", Some("Refactor"), DEFAULT_AGENT_ID)
            .expect("create");
        fx.store
            .append(&created.id, Message::user("hello"), SessionState::Idle)
            .expect("append");

        let reopened = fx.reopen();
        let detail = reopened
            .open(&created.id, SessionState::Idle)
            .expect("open");

        assert_eq!(detail.session.id, created.id);
        assert_eq!(detail.session.title, "Refactor");
        assert_eq!(detail.messages.len(), 1);
        assert_eq!(detail.messages[0].text, "hello");
    }

    /// The one invariant the module exists to protect: a process killed
    /// mid-turn must not come back claiming the turn is still running.
    #[test]
    fn a_restart_never_reports_a_session_as_running() {
        let fx = Fixture::new();
        let created = fx
            .store
            .create("p1", None, DEFAULT_AGENT_ID)
            .expect("create");
        fx.store
            .append(&created.id, Message::user("go"), SessionState::Running)
            .expect("append");

        // Whatever state was stamped on the returned summary, nothing about it
        // reached the document.
        let raw = fs::read_to_string(fx.document()).expect("document");
        assert!(
            !raw.contains("running"),
            "session state must never be persisted: {raw}"
        );

        let listed = fx.reopen().list("p1", &idle);
        assert_eq!(listed[0].state, SessionState::Idle);
    }

    #[test]
    fn the_first_user_message_names_an_unnamed_session() {
        let fx = Fixture::new();
        let created = fx
            .store
            .create("p1", None, DEFAULT_AGENT_ID)
            .expect("create");
        assert_eq!(created.title, DEFAULT_TITLE);

        let updated = fx
            .store
            .append(
                &created.id,
                Message::user("  Explain the   policy matrix\nplease  "),
                SessionState::Idle,
            )
            .expect("append");

        assert_eq!(updated.title, "Explain the policy matrix please");
    }

    #[test]
    fn a_session_the_user_named_keeps_its_name() {
        let fx = Fixture::new();
        let created = fx
            .store
            .create("p1", Some("Mine"), DEFAULT_AGENT_ID)
            .expect("create");

        let updated = fx
            .store
            .append(&created.id, Message::user("anything"), SessionState::Idle)
            .expect("append");

        assert_eq!(updated.title, "Mine");
    }

    #[test]
    fn a_long_first_message_is_cut_on_a_word_boundary() {
        let title = title_from(
            "Please read every file under src and tell me which of them still \
             mention the old name",
        );

        assert!(title.ends_with('…'), "{title}");
        assert!(title.chars().count() <= TITLE_MAX_CHARS + 1, "{title}");
        assert!(
            !title.trim_end_matches('…').ends_with(' '),
            "the ellipsis follows a word, not a space: {title}"
        );
        assert!(
            title.starts_with("Please read every file under src"),
            "{title}"
        );
    }

    /// A word longer than the budget has no boundary to fall back on; cutting
    /// it mid-word is the only option, and must not panic on a multi-byte
    /// character.
    #[test]
    fn a_title_never_splits_a_character() {
        let title = title_from(&"é".repeat(200));
        assert_eq!(title.chars().count(), TITLE_MAX_CHARS + 1);
    }

    #[test]
    fn sessions_list_most_recently_active_first() {
        let fx = Fixture::new();
        let first = fx
            .store
            .create("p1", Some("First"), DEFAULT_AGENT_ID)
            .expect("create");
        let second = fx
            .store
            .create("p1", Some("Second"), DEFAULT_AGENT_ID)
            .expect("create");
        let other = fx
            .store
            .create("p2", Some("Elsewhere"), DEFAULT_AGENT_ID)
            .expect("create");

        // Touching the older session moves it to the top.
        fx.store
            .append(&first.id, Message::user("later"), SessionState::Idle)
            .expect("append");

        let listed = fx.store.list("p1", &idle);
        let titles: Vec<&str> = listed.iter().map(|s| s.title.as_str()).collect();

        assert_eq!(titles, vec!["First", "Second"]);
        assert!(
            !listed.iter().any(|s| s.id == other.id),
            "another project's sessions are not this project's"
        );
        assert_eq!(listed[0].message_count, 1);
        assert_eq!(listed[1].id, second.id);
    }

    #[test]
    fn the_live_state_comes_from_the_caller() {
        let fx = Fixture::new();
        let created = fx
            .store
            .create("p1", None, DEFAULT_AGENT_ID)
            .expect("create");
        let id = created.id.clone();

        let running = |candidate: &str| {
            if candidate == id {
                SessionState::Running
            } else {
                SessionState::Idle
            }
        };

        assert_eq!(
            fx.store.list("p1", &running)[0].state,
            SessionState::Running
        );
    }

    #[test]
    fn renaming_refuses_an_empty_title() {
        let fx = Fixture::new();
        let created = fx
            .store
            .create("p1", Some("Keep"), DEFAULT_AGENT_ID)
            .expect("create");

        let err = fx
            .store
            .rename(&created.id, "   ", SessionState::Idle)
            .expect_err("an empty title is refused");
        assert!(matches!(err, AppError::SessionTitle), "{err:?}");

        let detail = fx
            .store
            .open(&created.id, SessionState::Idle)
            .expect("open");
        assert_eq!(detail.session.title, "Keep");
    }

    #[test]
    fn a_tool_call_status_can_be_advanced_without_losing_its_summary() {
        let fx = Fixture::new();
        let created = fx
            .store
            .create("p1", None, DEFAULT_AGENT_ID)
            .expect("create");

        fx.store
            .append(
                &created.id,
                Message::assistant(
                    "",
                    vec![ToolCallRecord {
                        call_id: "call_1".to_owned(),
                        tool: "fs_read".to_owned(),
                        args_json: r#"{"path":"a.txt"}"#.to_owned(),
                        status: ToolCallStatus::Pending,
                        summary: None,
                        image_path: None,
                        thought_signature: None,
                    }],
                ),
                SessionState::Running,
            )
            .expect("append");

        assert!(fx
            .store
            .set_tool_call_status(
                &created.id,
                "call_1",
                ToolCallStatus::Ok,
                Some("read a.txt (12 B)".to_owned()),
                None,
            )
            .expect("update"));

        // A later status change that carries no summary keeps the one on file.
        assert!(fx
            .store
            .set_tool_call_status(&created.id, "call_1", ToolCallStatus::Cancelled, None, None)
            .expect("update"));

        let detail = fx
            .store
            .open(&created.id, SessionState::Idle)
            .expect("open");
        let call = &detail.messages[0].tool_calls[0];
        assert_eq!(call.status, ToolCallStatus::Cancelled);
        assert_eq!(call.summary.as_deref(), Some("read a.txt (12 B)"));
    }

    #[test]
    fn updating_an_unknown_call_reports_it_rather_than_failing() {
        let fx = Fixture::new();
        let created = fx
            .store
            .create("p1", None, DEFAULT_AGENT_ID)
            .expect("create");

        let found = fx
            .store
            .set_tool_call_status(&created.id, "nope", ToolCallStatus::Ok, None, None)
            .expect("no error");
        assert!(!found);
    }

    #[test]
    fn deleting_a_project_takes_its_sessions_with_it() {
        let fx = Fixture::new();
        fx.store
            .create("p1", None, DEFAULT_AGENT_ID)
            .expect("create");
        fx.store
            .create("p1", None, DEFAULT_AGENT_ID)
            .expect("create");
        let kept = fx
            .store
            .create("p2", None, DEFAULT_AGENT_ID)
            .expect("create");

        assert_eq!(fx.store.delete_for_project("p1").expect("delete"), 2);
        assert!(fx.store.list("p1", &idle).is_empty());
        assert_eq!(fx.store.list("p2", &idle).len(), 1);
        assert!(fx.store.open(&kept.id, SessionState::Idle).is_ok());
    }

    #[test]
    fn an_unknown_session_is_a_stale_list_not_a_crash() {
        let fx = Fixture::new();

        let err = fx
            .store
            .open("missing", SessionState::Idle)
            .expect_err("no such session");
        assert!(matches!(err, AppError::SessionNotFound { .. }), "{err:?}");

        assert!(fx.store.delete("missing").is_err());
        assert!(fx.store.messages("missing").is_err());
        assert!(fx.store.project_of("missing").is_err());
    }

    #[test]
    fn a_damaged_document_is_moved_aside_rather_than_blocking_the_app() {
        let fx = Fixture::new();
        fx.store
            .create("p1", Some("Gone"), DEFAULT_AGENT_ID)
            .expect("create");

        fs::write(fx.document(), b"{ not json").expect("damage the document");
        let reopened = fx.reopen();

        assert!(reopened.list("p1", &idle).is_empty());
        let quarantined: Vec<_> = fs::read_dir(&fx.data)
            .expect("read data dir")
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains("corrupt-"))
            .collect();
        assert_eq!(quarantined.len(), 1, "the original is kept, not deleted");
    }

    #[test]
    fn a_future_schema_version_is_quarantined_rather_than_guessed_at() {
        let fx = Fixture::new();
        fs::write(
            fx.document(),
            br#"{"version":99,"sessions":[{"id":"s","project_id":"p","title":"t","created_at":"","updated_at":"","messages":[]}]}"#,
        )
        .expect("write a future document");

        assert!(fx.reopen().list("p", &idle).is_empty());
    }

    /// `src/ipc/bindings.ts` is generated from these structs, so a renamed
    /// field reaches TypeScript on its own. What generation cannot check is
    /// that the names still match the contract in `PLAN.md` § 2.1 — a rename
    /// would regenerate happily and silently change the wire format.
    #[test]
    fn payloads_carry_the_documented_field_names() {
        let detail = SessionDetail {
            session: SessionSummary {
                id: "s".to_owned(),
                project_id: "p".to_owned(),
                agent_id: DEFAULT_AGENT_ID.to_owned(),
                title: "First".to_owned(),
                created_at: "2026-08-28T09:41:07.412Z".to_owned(),
                updated_at: "2026-08-28T09:41:07.412Z".to_owned(),
                message_count: 3,
                state: SessionState::AwaitingApproval,
                delegated: None,
                scheduled: None,
                cost: Cost::default(),
            },
            messages: vec![Message::assistant(
                "done",
                vec![ToolCallRecord {
                    call_id: "call_1".to_owned(),
                    tool: "fs_read".to_owned(),
                    args_json: "{}".to_owned(),
                    status: ToolCallStatus::Ok,
                    summary: None,
                    image_path: None,
                    thought_signature: None,
                }],
            )],
            compaction: Some(Compaction {
                through_message_id: "m1".to_owned(),
                folded: 6,
                state: "Goal: ship it".to_owned(),
                at: "2026-08-28T09:41:07.412Z".to_owned(),
            }),
            pending_approvals: Vec::new(),
        };

        let json = serde_json::to_value(&detail).expect("SessionDetail serializes");

        let fields = |value: &serde_json::Value| -> Vec<String> {
            let mut keys: Vec<String> = value
                .as_object()
                .expect("a JSON object")
                .keys()
                .cloned()
                .collect();
            keys.sort();
            keys
        };
        let sorted = |names: &[&str]| -> Vec<String> {
            let mut out: Vec<String> = names.iter().map(|s| (*s).to_owned()).collect();
            out.sort();
            out
        };

        assert_eq!(
            fields(&json),
            sorted(&["session", "messages", "compaction", "pending_approvals"])
        );
        assert_eq!(
            fields(&json["compaction"]),
            sorted(&["through_message_id", "folded", "state", "at"])
        );
        assert_eq!(
            fields(&json["session"]),
            sorted(&[
                "id",
                "project_id",
                "agent_id",
                "title",
                "created_at",
                "updated_at",
                "message_count",
                "state",
                "delegated",
                "scheduled",
                "cost",
            ])
        );
        assert_eq!(
            fields(&json["messages"][0]),
            sorted(&[
                "id",
                "role",
                "text",
                "tool_calls",
                "tool_call_id",
                "created_at"
            ])
        );
        assert_eq!(
            fields(&json["messages"][0]["tool_calls"][0]),
            sorted(&[
                "call_id",
                "tool",
                "args_json",
                "status",
                "summary",
                "image_path"
            ])
        );

        // The enums cross the wire as the snake_case strings the UI branches on.
        assert_eq!(json["session"]["state"], "awaiting_approval");
        assert_eq!(json["messages"][0]["role"], "assistant");
        assert_eq!(json["messages"][0]["tool_calls"][0]["status"], "ok");

        // `None` must reach TypeScript as `null`, not as a missing key.
        assert!(json["messages"][0]["tool_call_id"].is_null());
        assert!(json["messages"][0]["tool_calls"][0]["summary"].is_null());
    }

    #[test]
    fn a_turn_handle_carries_both_ids() {
        let json = serde_json::to_value(TurnHandle {
            session_id: "s".to_owned(),
            turn_id: "t".to_owned(),
        })
        .expect("TurnHandle serializes");

        assert_eq!(json["session_id"], "s");
        assert_eq!(json["turn_id"], "t");
    }
}
