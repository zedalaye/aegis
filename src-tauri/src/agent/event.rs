//! The events a turn emits (PLAN 2.2).
//!
//! Typed payloads, generated into `src/ipc/bindings.ts`.
//!
//! * **No secrets**: tool arguments are redacted like the audit log's.
//! * **Every payload carries `session_id`**, `tool:finished` included.
//!
//! Emission goes through [`EventSink`] so the turn loop runs in tests without
//! Tauri; the window's sink is `commands::session::WindowSink`.

use serde::Serialize;
use serde_json::Value;
use ts_rs::TS;

use crate::agent::decision::tool_risk::RiskAnnotation;
use crate::approval::{ApprovalRequest, Decision, ResolvedBy};
use crate::audit::{AuditEntry, Outcome};
use crate::mcp::ConnectorView;
use crate::store::{Message, Routine, SessionSummary};
use crate::tools::Stream;

use super::wire::{StopReason, Usage};

/// Event names. `domain:verb`, past tense (PLAN 2.2).
pub mod name {
    /// A turn began.
    pub const TURN_STARTED: &str = "turn:started";
    /// More assistant text.
    pub const TURN_DELTA: &str = "turn:delta";
    /// An assistant message was finalized and persisted.
    pub const TURN_MESSAGE: &str = "turn:message";
    /// A turn ended, for any reason.
    pub const TURN_FINISHED: &str = "turn:finished";
    /// A turn failed.
    pub const TURN_ERROR: &str = "turn:error";
    /// The model asked for a tool.
    pub const TOOL_REQUESTED: &str = "tool:requested";
    /// A tool call is blocked on a human.
    pub const TOOL_APPROVAL_REQUIRED: &str = "tool:approval_required";
    /// The decision model annotated a pending approval (PLAN 7.18).
    pub const TOOL_APPROVAL_ANNOTATED: &str = "tool:approval_annotated";
    /// An approval was answered, however it was answered.
    pub const TOOL_APPROVAL_RESOLVED: &str = "tool:approval_resolved";
    /// A tool began running.
    pub const TOOL_STARTED: &str = "tool:started";
    /// The model is still writing a tool call's arguments.
    pub const TOOL_DRAFTING: &str = "tool:drafting";
    /// A running tool produced output.
    pub const TOOL_PROGRESS: &str = "tool:progress";
    /// A tool finished, whatever became of it.
    pub const TOOL_FINISHED: &str = "tool:finished";
    /// A session's row changed.
    pub const SESSION_UPDATED: &str = "session:updated";
    /// A line was appended to the audit log.
    pub const AUDIT_APPENDED: &str = "audit:appended";
    /// A routine's row changed.
    pub const ROUTINE_UPDATED: &str = "routine:updated";
    /// A connector's row changed.
    pub const CONNECTOR_UPDATED: &str = "connector:updated";
    /// The OS dropped files on the window.
    pub const WORKSPACE_DROPPED: &str = "workspace:dropped";
}

/// `turn:started`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct TurnStarted {
    /// The session the turn runs in.
    pub session_id: String,
    /// The turn.
    pub turn_id: String,
    /// Which model is answering.
    pub model: String,
}

/// `turn:delta` — a frame of assistant text.
///
/// Coalesced into ~50 ms frames (PLAN 4.2), not one per token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct TurnDelta {
    /// The session.
    pub session_id: String,
    /// The turn.
    pub turn_id: String,
    /// Per-turn monotonic counter. The UI drops anything out of order or
    /// repeated, which is what makes re-syncing after a reload safe.
    pub seq: u32,
    /// The text to append.
    pub text: String,
}

/// `turn:message` — an assistant message, finalized and on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct TurnMessage {
    /// The session.
    pub session_id: String,
    /// The turn.
    pub turn_id: String,
    /// The message as it was persisted. Replaces the streamed buffer.
    pub message: Message,
}

/// `turn:finished`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct TurnFinished {
    /// The session.
    pub session_id: String,
    /// The turn.
    pub turn_id: String,
    /// Why it ended.
    pub stop_reason: StopReason,
    /// What it cost, when the provider said.
    pub usage: Option<Usage>,
}

/// `turn:error`.
///
/// Always followed by a `turn:finished` carrying [`StopReason::Error`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct TurnError {
    /// The session.
    pub session_id: String,
    /// The turn.
    pub turn_id: String,
    /// A stable code from PLAN 4.4.
    pub code: String,
    /// What went wrong, written for the user.
    pub message: String,
    /// Whether sending the same thing again could work.
    pub retryable: bool,
}

/// `tool:requested` — the model asked, before policy has judged it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct ToolRequested {
    /// The session.
    pub session_id: String,
    /// The turn.
    pub turn_id: String,
    /// The model's id for the call.
    pub call_id: String,
    /// The tool name.
    pub tool: String,
    /// The arguments, redacted the way the audit log redacts them: paths kept
    /// whole, long values shortened, file content replaced by its size.
    pub args_redacted: String,
}

/// `tool:approval_resolved` — an approval stopped being pending.
///
/// Emitted however it ended — answer, cancel or expiry — per `resolved_by`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct ToolApprovalResolved {
    /// The session.
    pub session_id: String,
    /// The turn.
    pub turn_id: String,
    /// The request that was answered.
    pub request_id: String,
    /// The call it was about.
    pub call_id: String,
    /// What was decided.
    pub decision: Decision,
    /// Who decided it.
    pub resolved_by: ResolvedBy,
}

/// `tool:started` — policy cleared it and it is running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct ToolStarted {
    /// The session.
    pub session_id: String,
    /// The turn.
    pub turn_id: String,
    /// The call.
    pub call_id: String,
    /// The tool name.
    pub tool: String,
}

/// `tool:drafting` — the model is still writing a call's arguments.
///
/// The input-side mirror of [`ToolProgress`]: a large `fs_write` streams
/// minutes of arguments with no text, which looks like a hang. Carries a byte
/// count (partial JSON is unreadable), coalesced like `turn:delta`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct ToolDrafting {
    /// The session.
    pub session_id: String,
    /// The turn.
    pub turn_id: String,
    /// Which call within this response. There is no call id yet — the model
    /// may not have sent one, and the call does not exist until the arguments
    /// parse.
    pub index: u32,
    /// The tool being called, once the model has named it. `None` while the
    /// only thing that has arrived is arguments.
    pub tool: Option<String>,
    /// Per-turn monotonic counter, as on `turn:delta`. The UI drops anything
    /// out of order or repeated.
    pub seq: u32,
    /// Bytes of arguments accumulated for this call so far.
    #[ts(type = "number")]
    pub bytes: u64,
}

/// `tool:progress` — output from a tool that is still running.
///
/// `shell_exec` only (PLAN 2.2). A coalesced, capped live view, not a record;
/// `tool:finished.truncated` says the pane stopped short.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct ToolProgress {
    /// The session.
    pub session_id: String,
    /// The turn.
    pub turn_id: String,
    /// The call.
    pub call_id: String,
    /// Which pipe this came from.
    pub stream: Stream,
    /// Per-turn monotonic counter, as on `turn:delta`. The UI drops anything
    /// out of order or repeated.
    pub seq: u32,
    /// The text to append.
    pub chunk: String,
}

/// `tool:finished` — the call ended, whatever became of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct ToolFinished {
    /// The session.
    pub session_id: String,
    /// The turn.
    pub turn_id: String,
    /// The call.
    pub call_id: String,
    /// How it ended.
    pub outcome: Outcome,
    /// The one human line. Never a raw blob.
    pub summary: String,
    /// How long the execution itself took.
    #[ts(type = "number")]
    pub duration_ms: u64,
    /// Whether the result the model saw was shorter than what was available.
    pub truncated: bool,
    /// A capture's path (PLAN 5.4), on the event so the thumbnail shows
    /// immediately.
    pub image_path: Option<String>,
}

/// `tool:approval_annotated` — a pending approval gained a risk annotation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct ToolApprovalAnnotated {
    /// The session.
    pub session_id: String,
    /// The request it belongs to.
    pub request_id: String,
    /// What the decision model said.
    pub annotation: RiskAnnotation,
}

/// One event, ready to emit.
///
/// One enum, so [`EventSink`] has a single method.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// `turn:started`.
    TurnStarted(TurnStarted),
    /// `turn:delta`.
    TurnDelta(TurnDelta),
    /// `turn:message`.
    TurnMessage(Box<TurnMessage>),
    /// `turn:finished`.
    TurnFinished(TurnFinished),
    /// `turn:error`.
    TurnError(TurnError),
    /// `tool:requested`.
    ToolRequested(ToolRequested),
    /// `tool:approval_required`.
    ToolApprovalRequired(Box<ApprovalRequest>),
    /// `tool:approval_annotated`.
    ToolApprovalAnnotated(Box<ToolApprovalAnnotated>),
    /// `tool:approval_resolved`.
    ToolApprovalResolved(ToolApprovalResolved),
    /// `tool:started`.
    ToolStarted(ToolStarted),
    /// `tool:progress`.
    ToolDrafting(ToolDrafting),
    ToolProgress(ToolProgress),
    /// `tool:finished`.
    ToolFinished(ToolFinished),
    /// `session:updated`.
    SessionUpdated(Box<SessionSummary>),
    /// `audit:appended`.
    AuditAppended(Box<AuditEntry>),
    /// `routine:updated` (Phase 16): a routine's row changed, e.g. a run ended.
    RoutineUpdated(Box<Routine>),
    /// `connector:updated` (Phase 18): a connector's state changed.
    ConnectorUpdated(Box<ConnectorView>),
    /// `workspace:dropped` (PLAN 7.15): a drop is held under an id; paths stay
    /// in the runtime.
    WorkspaceDropped(crate::intake::WorkspaceDropped),
}

impl Event {
    /// The event name this payload is emitted under.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::TurnStarted(_) => name::TURN_STARTED,
            Self::TurnDelta(_) => name::TURN_DELTA,
            Self::TurnMessage(_) => name::TURN_MESSAGE,
            Self::TurnFinished(_) => name::TURN_FINISHED,
            Self::TurnError(_) => name::TURN_ERROR,
            Self::ToolRequested(_) => name::TOOL_REQUESTED,
            Self::ToolApprovalRequired(_) => name::TOOL_APPROVAL_REQUIRED,
            Self::ToolApprovalAnnotated(_) => name::TOOL_APPROVAL_ANNOTATED,
            Self::ToolApprovalResolved(_) => name::TOOL_APPROVAL_RESOLVED,
            Self::ToolStarted(_) => name::TOOL_STARTED,
            Self::ToolDrafting(_) => name::TOOL_DRAFTING,
            Self::ToolProgress(_) => name::TOOL_PROGRESS,
            Self::ToolFinished(_) => name::TOOL_FINISHED,
            Self::SessionUpdated(_) => name::SESSION_UPDATED,
            Self::AuditAppended(_) => name::AUDIT_APPENDED,
            Self::RoutineUpdated(_) => name::ROUTINE_UPDATED,
            Self::ConnectorUpdated(_) => name::CONNECTOR_UPDATED,
            Self::WorkspaceDropped(_) => name::WORKSPACE_DROPPED,
        }
    }

    /// Which session this belongs to, so a sink can route or filter it.
    pub fn session_id(&self) -> &str {
        match self {
            Self::TurnStarted(payload) => &payload.session_id,
            Self::TurnDelta(payload) => &payload.session_id,
            Self::TurnMessage(payload) => &payload.session_id,
            Self::TurnFinished(payload) => &payload.session_id,
            Self::TurnError(payload) => &payload.session_id,
            Self::ToolRequested(payload) => &payload.session_id,
            Self::ToolApprovalRequired(payload) => &payload.session_id,
            Self::ToolApprovalAnnotated(payload) => &payload.session_id,
            Self::ToolApprovalResolved(payload) => &payload.session_id,
            Self::ToolStarted(payload) => &payload.session_id,
            Self::ToolDrafting(payload) => &payload.session_id,
            Self::ToolProgress(payload) => &payload.session_id,
            Self::ToolFinished(payload) => &payload.session_id,
            Self::SessionUpdated(payload) => &payload.id,
            Self::AuditAppended(payload) => &payload.session_id,
            // Routed to the session its last run opened; empty if none.
            Self::RoutineUpdated(payload) => payload
                .last
                .as_ref()
                .map_or("", |last| last.session_id.as_str()),
            // A connector belongs to the installation, not to a session. The
            // empty id matches nothing, which is what a sink filtering by
            // session should do with it.
            Self::ConnectorUpdated(_) => "",
            // A drop lands on the window, not in a conversation.
            Self::WorkspaceDropped(_) => "",
        }
    }

    /// The payload as JSON.
    ///
    /// Falls back to `null` and a log line rather than failing the turn.
    pub fn payload(&self) -> Value {
        let rendered = match self {
            Self::TurnStarted(payload) => serde_json::to_value(payload),
            Self::TurnDelta(payload) => serde_json::to_value(payload),
            Self::TurnMessage(payload) => serde_json::to_value(payload),
            Self::TurnFinished(payload) => serde_json::to_value(payload),
            Self::TurnError(payload) => serde_json::to_value(payload),
            Self::ToolRequested(payload) => serde_json::to_value(payload),
            Self::ToolApprovalRequired(payload) => serde_json::to_value(payload),
            Self::ToolApprovalAnnotated(payload) => serde_json::to_value(payload),
            Self::ToolApprovalResolved(payload) => serde_json::to_value(payload),
            Self::ToolStarted(payload) => serde_json::to_value(payload),
            Self::ToolDrafting(payload) => serde_json::to_value(payload),
            Self::ToolProgress(payload) => serde_json::to_value(payload),
            Self::ToolFinished(payload) => serde_json::to_value(payload),
            Self::SessionUpdated(payload) => serde_json::to_value(payload),
            Self::AuditAppended(payload) => serde_json::to_value(payload),
            Self::RoutineUpdated(payload) => serde_json::to_value(payload),
            Self::ConnectorUpdated(payload) => serde_json::to_value(payload),
            Self::WorkspaceDropped(payload) => serde_json::to_value(payload),
        };

        rendered.unwrap_or_else(|err| {
            tracing::error!(%err, event = self.name(), "an event payload would not serialize");
            Value::Null
        })
    }
}

/// Where a turn's events go.
///
/// The turn loop never sees an `AppHandle`, so it runs in unit tests.
pub trait EventSink: Send + Sync {
    /// Delivers one event. Never fails: a UI that missed a frame is a
    /// cosmetic problem, and a turn that aborted because the window was
    /// closing is not.
    fn emit(&self, event: Event);
}

/// An [`EventSink`] that drops everything, for turns nobody watches.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullSink;

impl EventSink for NullSink {
    fn emit(&self, _event: Event) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_documented_convention() {
        let events = [
            Event::TurnStarted(TurnStarted {
                session_id: "s".to_owned(),
                turn_id: "t".to_owned(),
                model: "m".to_owned(),
            }),
            Event::TurnDelta(TurnDelta {
                session_id: "s".to_owned(),
                turn_id: "t".to_owned(),
                seq: 0,
                text: "hi".to_owned(),
            }),
        ];

        for event in events {
            let name = event.name();
            assert!(name.contains(':'), "{name} is not `domain:verb`");
            assert_eq!(name.to_lowercase(), name, "{name} is not lowercase");
            assert_eq!(event.session_id(), "s");
        }
    }

    /// The invariant of PLAN 2.2: a window showing one session must be able to
    /// ignore another's traffic without knowing what the event means.
    #[test]
    fn every_payload_carries_its_session() {
        let event = Event::ToolFinished(ToolFinished {
            session_id: "s7".to_owned(),
            turn_id: "t".to_owned(),
            call_id: "c".to_owned(),
            outcome: Outcome::Ok,
            summary: "read a.txt".to_owned(),
            duration_ms: 3,
            truncated: false,
            image_path: None,
        });

        assert_eq!(event.session_id(), "s7");
        assert_eq!(event.payload()["session_id"], "s7");
    }

    #[test]
    fn a_delta_renders_the_documented_fields() {
        let payload = Event::TurnDelta(TurnDelta {
            session_id: "s".to_owned(),
            turn_id: "t".to_owned(),
            seq: 4,
            text: "hello ".to_owned(),
        })
        .payload();

        assert_eq!(payload["seq"], 4);
        assert_eq!(
            payload["text"], "hello ",
            "whitespace is content and must not be trimmed"
        );
    }

    #[test]
    fn progress_names_its_pipe_on_the_wire() {
        let payload = Event::ToolProgress(ToolProgress {
            session_id: "s".to_owned(),
            turn_id: "t".to_owned(),
            call_id: "c".to_owned(),
            stream: Stream::Stderr,
            seq: 2,
            chunk: "warning: ".to_owned(),
        })
        .payload();

        assert_eq!(payload["stream"], "stderr");
        assert_eq!(payload["seq"], 2);
        assert_eq!(
            payload["chunk"], "warning: ",
            "whitespace is content and must not be trimmed"
        );
    }

    #[test]
    fn a_finish_carries_the_stop_reason_as_its_wire_string() {
        let payload = Event::TurnFinished(TurnFinished {
            session_id: "s".to_owned(),
            turn_id: "t".to_owned(),
            stop_reason: StopReason::Cancelled,
            usage: None,
        })
        .payload();

        assert_eq!(payload["stop_reason"], "cancelled");
        assert!(payload["usage"].is_null());
    }
}
