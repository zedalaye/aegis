//! The session IPC payloads (PLAN 2.1, "Sessions and turns"), exported to
//! `bindings.ts`.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::*;

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

/// An image the operator attached to a message (PLAN 7.20).
///
/// The file lives under the app's own attachment directory, never the
/// workspace; the window draws it through the scoped `asset:` protocol and the
/// runtime sends its pixels to the model. Only the path is ever stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct Attachment {
    /// Absolute path of the copy under the attachment directory.
    pub path: String,
    /// The type its bytes declare: `image/png`, `image/jpeg`, …
    pub mime: String,
    /// Width in pixels, when it was read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub width: Option<u32>,
    /// Height in pixels, when it was read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub height: Option<u32>,
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
    /// Images the operator attached. `user` messages only (PLAN 7.20); absent
    /// on disk when there are none, so the document version stays 1.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<Attachment>>")]
    pub attachments: Vec<Attachment>,
    /// RFC3339, UTC.
    pub created_at: String,
}

impl Message {
    /// A message from the person.
    pub fn user(text: impl Into<String>) -> Self {
        Self::new(Role::User, text.into())
    }

    /// A message from the person, with the images they attached.
    pub fn user_with(text: impl Into<String>, attachments: Vec<Attachment>) -> Self {
        Self {
            attachments,
            ..Self::new(Role::User, text.into())
        }
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
            attachments: Vec::new(),
            created_at: now(),
        }
    }

    /// Whether this message carries nothing, and so is not recorded.
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.tool_calls.is_empty() && self.attachments.is_empty()
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
    /// The provider row this session answers from instead of its identity's
    /// (PLAN 7.19). `None` inherits.
    pub provider_id: Option<String>,
    /// The model this session sends instead of the inherited one. `None`
    /// inherits.
    pub model: Option<String>,
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
    /// What the turn cost in micro-dollars, as the spend ledger priced it
    /// (PLAN 7.26). `None` when its model has no price. A copy for display:
    /// caps are enforced from the ledger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null")]
    pub micros: Option<u64>,
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
            micros: None,
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

    /// What the turn cost in money, when its model has a price.
    pub const fn with_micros(mut self, micros: Option<u64>) -> Self {
        self.micros = micros;
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
            micros: None,
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
    /// Micro-dollars, over the turns that had a price (PLAN 7.26).
    #[serde(default)]
    #[ts(type = "number")]
    pub micros: u64,
    /// Of the turns summed, how many had a price. Fewer than `turns` means
    /// `micros` is "at least".
    #[serde(default)]
    pub priced: u32,
}

impl Cost {
    /// Sums the turns an iterator yields.
    pub fn of<'a>(turns: impl IntoIterator<Item = &'a TurnCost>) -> Self {
        turns.into_iter().fold(Self::default(), |mut cost, turn| {
            cost.turns = cost.turns.saturating_add(1);
            if let Some(micros) = turn.micros {
                cost.micros = cost.micros.saturating_add(micros);
                cost.priced = cost.priced.saturating_add(1);
            }
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
        self.micros = self.micros.saturating_add(other.micros);
        self.priced = self.priced.saturating_add(other.priced);
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
