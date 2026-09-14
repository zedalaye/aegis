//! The session document: `sessions.json`.
//!
//! A session is a conversation in a project: title, transcript, timestamps.
//! Whether a turn is running is never persisted — [`SessionState`] is derived
//! from the turn registry on read — so a session interrupted by a crash comes
//! back idle.
//!
//! A [`Compaction`] (Phase 14) is a pointer plus derived state, never a
//! deletion: the transcript stays whole, and only what [`compact::tail`] hands
//! the model changes.
//!
//! Each finished turn's [`TurnCost`] (Phase 17) is stored here rather than on
//! the audit line: tokens are spent per model round, and a turn that called no
//! tool still spends them. A run's cost joins the two on `turn_id`
//! ([`board::trace`](crate::board::trace)).
//!
//! One document holds every session; [`SessionStore::save`] is the seam if a
//! transcript ever needs a file of its own.

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

/// Schema version of [`SessionsFile`], independent of the project document's.
const SCHEMA_VERSION: u32 = 1;

/// Title given to a session created without one.
const DEFAULT_TITLE: &str = "New session";

/// How much of the first user message becomes the session title.
const TITLE_MAX_CHARS: usize = 48;

// ---------------------------------------------------------------------------
// IPC payloads (PLAN 2.1, "Sessions and turns")
// ---------------------------------------------------------------------------

/// Lifecycle of a session, derived on every read and never stored.
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
    /// The runtime's instructions. Never persisted: rebuilt for every request.
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

/// One tool call as the transcript records it: `args_json` verbatim as the
/// model sent it, `summary` one human line, never a blob.
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
    /// A capture's path for the transcript (PLAN 5.4), persisted so a reopened
    /// session shows it. Defaulted for older transcripts.
    #[serde(default)]
    pub image_path: Option<String>,
    /// Gemini's thought signature, echoed on the next request. Opaque, kept on
    /// disk across restarts, never sent to the WebView.
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

    /// Whether this message carries nothing, and so is not recorded.
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
    /// The identity this session runs as (Phase 12). Always concrete: a session
    /// from before identities resolves to
    /// [`DEFAULT_AGENT_ID`](super::agents::DEFAULT_AGENT_ID).
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
    /// The brief that opened this session (Phase 15), drawn as a badge.
    pub delegated: Option<Delegated>,
    /// The routine that opened this session (Phase 16). Its own field: a
    /// routine's run is not a brief.
    pub scheduled: Option<Scheduled>,
    /// What the conversation spent (Phase 17), summed on read, never stored.
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
    pub compaction: Option<Compaction>,
    /// Approvals this session is blocked on, filled by
    /// [`AppState::session_detail`](crate::AppState::session_detail) so a
    /// reopened window redraws the dialog.
    pub pending_approvals: Vec<ApprovalRequest>,
}

/// Why a session exists when a brief opened it (Phase 15): which delegation,
/// which session delegated, where the brief was filed. Otherwise an ordinary
/// session, visible in the sidebar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct Delegated {
    /// The delegation this run belongs to. Shared with the audit lines.
    pub handoff_id: String,
    /// The session whose turn handed the brief out.
    pub from_session_id: String,
    /// Where the brief was filed, relative to the workspace. `None` without
    /// `.aegis/briefs/`: the brief is then only the first message.
    pub brief: Option<String>,
}

/// Why a session exists when a routine opened it (Phase 16). The routine's name
/// is copied so the row still explains itself after a rename or deletion.
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

/// What a session has folded (Phase 14): a pointer and derived state, never a
/// deletion ([`compact`](crate::compact)).
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

/// What one finished turn spent, as the provider reported it (Phase 17),
/// whatever the turn did. `turn_id` joins it to the audit lines;
/// `reported: false` means unknown, not free.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct TurnCost {
    /// The turn this is the cost of.
    pub turn_id: String,
    /// Tokens in the requests this turn made, cached ones included.
    #[ts(type = "number")]
    pub prompt_tokens: u64,
    /// Of those, how many were served from the prompt cache. Defaulted for
    /// older records.
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
    /// A turn whose provider reported usage, summed over the turn's requests.
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

    /// How much of [`Self::prompt_tokens`] went through the cache. Zero is honest
    /// for a provider that reports usage without cache figures.
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

/// Tokens spent over some turns (Phase 17). `unreported` is kept apart so a
/// total can read "at least".
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

    /// Whether nothing is known: a board then draws nothing, not "0 tokens".
    pub const fn is_empty(&self) -> bool {
        self.turns == 0
    }
}

/// What `session_send` returns (PLAN 2.1): the handle to cancel the turn, which
/// reports through events.
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

/// A session record as persisted. Not [`SessionSummary`], so derived fields
/// cannot be persisted by accident. Every later field is `#[serde(default)]`,
/// which is how older documents keep loading.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredSession {
    id: String,
    project_id: String,
    /// The bound identity; `None` before Phase 12, resolving to the built-in
    /// one.
    #[serde(default)]
    agent_id: Option<String>,
    title: String,
    created_at: String,
    updated_at: String,
    #[serde(default)]
    messages: Vec<Message>,
    /// What has been folded, if anything.
    #[serde(default)]
    compaction: Option<Compaction>,
    /// The brief that opened this session, if one did.
    #[serde(default)]
    delegated: Option<Delegated>,
    /// The routine that opened this session, if one did.
    #[serde(default)]
    scheduled: Option<Scheduled>,
    /// What each finished turn spent, oldest first. Per turn, because a run
    /// covers only some of a session's turns.
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

/// The session store: one mutex over the list, written out on every mutation.
/// Methods that produce a [`SessionSummary`] take its state from the caller,
/// since the store cannot see the turn registry.
#[derive(Debug)]
pub struct SessionStore {
    path: PathBuf,
    sessions: Mutex<Vec<StoredSession>>,
}

impl SessionStore {
    /// Loads the store from `data_dir`. Never fails: an unreadable document
    /// starts empty.
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

    /// Locks the list, recovering from poison: it cannot be left torn.
    fn sessions(&self) -> MutexGuard<'_, Vec<StoredSession>> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Creates a session in `project_id`, bound to `agent_id` for good: a
    /// transcript is the record of what that identity did. An empty title
    /// becomes [`DEFAULT_TITLE`] until the first user message
    /// ([`SessionStore::append`]). The caller checks the identity exists
    /// ([`AppState::create_session`](crate::AppState::create_session)).
    pub fn create(
        &self,
        project_id: &str,
        title: Option<&str>,
        agent_id: &str,
    ) -> AppResult<SessionSummary> {
        self.open_session(project_id, title, agent_id, None, None)
    }

    /// Creates the session a brief opens (Phase 15): an ordinary session with a
    /// [`Delegated`] on it.
    pub fn create_delegated(
        &self,
        project_id: &str,
        title: Option<&str>,
        agent_id: &str,
        delegated: Delegated,
    ) -> AppResult<SessionSummary> {
        self.open_session(project_id, title, agent_id, Some(delegated), None)
    }

    /// Creates the session a routine opens (Phase 16), with a [`Scheduled`] on
    /// it.
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

    /// A project's sessions, most recently active first, stamped by `state_of`.
    /// Fixed-width UTC RFC3339 strings sort as instants.
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

    /// The transcript and its compaction (Phase 14), under one lock so they
    /// match.
    pub fn context(&self, id: &str) -> AppResult<(Vec<Message>, Option<Compaction>)> {
        let sessions = self.sessions();
        let session = Self::find(&sessions, id)?;
        Ok((session.messages.clone(), session.compaction.clone()))
    }

    /// Folds the older part of a transcript, returning the new compaction or
    /// `None` when nothing moved (too few turns, or the same cut as before).
    /// `force` is the button; otherwise the transcript must pass
    /// [`compact::COMPACT_AT_BYTES`]. The state is derived from the messages
    /// every time, never from a previous fold. `updated_at` is left alone.
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

    /// Records what a turn spent (Phase 17), for every ending. Charging the same
    /// turn again replaces the figure. `updated_at` is left alone.
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

    /// Each turn's cost, oldest first, joined to runs by
    /// [`board::trace`](crate::board::trace).
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

    /// Updates one tool call by `call_id`, returning whether it was found. A
    /// `None` summary or image keeps what an earlier update recorded.
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

    /// Deletes every session of a forgotten project, returning how many went.
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

/// A session title from the first user message: whitespace collapsed, cut at
/// the last word boundary in the final quarter of the budget.
fn title_from(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return DEFAULT_TITLE.to_owned();
    }

    // By character, so a multi-byte character is never split.
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

    /// Generated bindings follow a rename silently; this pins the wire names
    /// to PLAN 2.1.
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
