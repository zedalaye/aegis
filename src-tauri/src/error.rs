//! The error type every IPC command returns.
//!
//! Two rules shape this module (AGENTS.md):
//!
//! * library paths never `unwrap` — everything fallible surfaces as
//!   [`AppError`] and crosses the IPC boundary as a structured payload;
//! * the WebView learns *what* failed, never *how the machine is laid out* —
//!   messages are written for a user, and nothing secret is ever formatted
//!   into one.
//!
//! Codes are the stable half of the contract (PLAN 4.4). The UI branches on
//! [`ErrorCode`]; the message is free to be reworded.

use std::fmt;

use serde::{Serialize, Serializer};

/// Stable, machine-readable failure codes, shared by IPC results and by the
/// `ToolResult` envelopes the model sees (PLAN 4.4).
///
/// The full set is declared here from Phase 1 so later phases add behaviour,
/// never a new vocabulary the UI has to learn. Two codes are additions to the
/// PLAN list, and both are deliberate:
///
/// * [`ErrorCode::Internal`] is the catch-all for a runtime failure that is
///   not part of the agent/tool domain — a missing window, a broken event
///   channel — and it is never a code the UI should branch on.
/// * [`ErrorCode::InvalidSetting`] arrived with the settings panel in Phase 8.
///   PLAN 4.4 covers the agent, the tools and the provider; it has no code for
///   "the value you just typed cannot be used", which is a different thing
///   from a runtime failure and needs a different answer from the UI — mark
///   the field, keep the form open. No screen written before Phase 8 has to
///   learn it, because the only screen that can raise it is the new one. The
///   agent form of Phase 12 raises the same code for the same reason: a second
///   vocabulary for "this field is wrong" would be a second thing every form
///   has to learn in order to behave the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    /// A turn is already running in this session.
    TurnBusy,
    /// The session has no workspace, so no path can be resolved.
    NoWorkspace,
    /// The resolved path escapes the workspace.
    PathOutsideWorkspace,
    /// The path is malformed, or canonicalization failed.
    PathInvalid,
    /// The user, or policy, refused the tool call.
    Denied,
    /// This tool may never be granted for a whole session.
    GrantNotAllowed,
    /// The approval request is unknown, already resolved, or expired.
    ApprovalStale,
    /// A tool exceeded its deadline.
    Timeout,
    /// The tool ran and failed on its own terms.
    ToolFailed,
    /// The provider answered with a non-success HTTP status.
    ProviderHttp,
    /// The provider's response could not be parsed.
    ProviderParse,
    /// No API key in the keyring or the environment.
    NoApiKey,
    /// No usable OS keyring (headless Linux, locked login keychain).
    KeyringUnavailable,
    /// The turn or tool call was cancelled.
    Cancelled,
    /// The turn loop hit its tool-round ceiling.
    TooManyToolRounds,
    /// Screen capture is not permitted, or the session type forbids it.
    ScreenPermission,
    /// A settings value cannot be used, and the user has to change it.
    InvalidSetting,
    /// Runtime failure outside the agent/tool domain.
    Internal,
}

impl ErrorCode {
    /// The wire representation. These strings are part of the IPC contract.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TurnBusy => "E_TURN_BUSY",
            Self::NoWorkspace => "E_NO_WORKSPACE",
            Self::PathOutsideWorkspace => "E_PATH_OUTSIDE_WORKSPACE",
            Self::PathInvalid => "E_PATH_INVALID",
            Self::Denied => "E_DENIED",
            Self::GrantNotAllowed => "E_GRANT_NOT_ALLOWED",
            Self::ApprovalStale => "E_APPROVAL_STALE",
            Self::Timeout => "E_TIMEOUT",
            Self::ToolFailed => "E_TOOL_FAILED",
            Self::ProviderHttp => "E_PROVIDER_HTTP",
            Self::ProviderParse => "E_PROVIDER_PARSE",
            Self::NoApiKey => "E_NO_API_KEY",
            Self::KeyringUnavailable => "E_KEYRING_UNAVAILABLE",
            Self::Cancelled => "E_CANCELLED",
            Self::TooManyToolRounds => "E_TOO_MANY_TOOL_ROUNDS",
            Self::ScreenPermission => "E_SCREEN_PERMISSION",
            Self::InvalidSetting => "E_INVALID_SETTING",
            Self::Internal => "E_INTERNAL",
        }
    }

    /// Whether repeating the identical request could plausibly succeed.
    ///
    /// A denial or a containment violation is a decision, not a hiccup:
    /// retrying it unchanged is always wrong, and the UI uses this to decide
    /// whether to offer a retry affordance at all.
    pub const fn retryable(self) -> bool {
        match self {
            Self::TurnBusy | Self::Timeout | Self::ProviderHttp | Self::ProviderParse => true,
            Self::NoWorkspace
            | Self::PathOutsideWorkspace
            | Self::PathInvalid
            | Self::Denied
            | Self::GrantNotAllowed
            | Self::ApprovalStale
            | Self::ToolFailed
            | Self::NoApiKey
            | Self::KeyringUnavailable
            | Self::Cancelled
            | Self::TooManyToolRounds
            | Self::ScreenPermission
            | Self::InvalidSetting
            | Self::Internal => false,
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for ErrorCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// Every failure a Tauri command can hand back to the WebView.
///
/// Each variant maps onto an existing [`ErrorCode`]; a new failure mode adds a
/// variant here, never a code.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// The named window is gone — closed, or never created.
    #[error("window `{label}` is not available")]
    WindowUnavailable {
        /// The window label that was looked up.
        label: String,
    },

    /// A Tauri runtime call failed.
    #[error("runtime error: {0}")]
    Runtime(#[from] tauri::Error),

    /// A path offered as a workspace cannot be used as one.
    ///
    /// The path is echoed back deliberately — the user just chose it, so it is
    /// theirs already, and naming it is what makes the message actionable.
    #[error("`{path}` cannot be used as a workspace: {reason}")]
    WorkspacePath {
        /// The path as it was given.
        path: String,
        /// Why it was refused, in words a user can act on.
        reason: String,
    },

    /// Part of the shared-workspace convention could not be created
    /// (PLAN 7.3, Phase 11).
    ///
    /// Scaffolding writes into the user's own folder, so its failures are
    /// theirs to see and to fix: a read-only checkout, a name already taken by
    /// a file where a directory has to go, a `briefs` symlink pointing off the
    /// disk. The path is relative to the workspace root, because that is how
    /// the convention is written down and where the user will go looking.
    #[error("could not create `{path}` in the workspace: {reason}")]
    WorkspaceScaffold {
        /// The convention path, relative to the workspace root.
        path: String,
        /// Why it could not be created, in words a user can act on.
        reason: String,
    },

    /// No project carries that id, so the UI is holding a stale list.
    ///
    /// The right response is to refetch rather than to branch on this: the
    /// project may have been deleted, or the store may have been reset.
    #[error("that project no longer exists")]
    ProjectNotFound {
        /// The id that was looked up. Logged, not shown.
        id: String,
    },

    /// No session carries that id, so the UI is holding a stale list.
    ///
    /// Same shape and same reasoning as [`AppError::ProjectNotFound`]: the
    /// right response is to refetch, not to branch on the code.
    #[error("that session no longer exists")]
    SessionNotFound {
        /// The id that was looked up. Logged, not shown.
        id: String,
    },

    /// A rename arrived with nothing in it.
    ///
    /// The composer disables its own save button on an empty field, so this is
    /// the defensive half of that check rather than a path a user reaches by
    /// typing — which is why it carries no dedicated code. A session with a
    /// blank title is a row in the sidebar that cannot be clicked on by name.
    #[error("a session needs a title")]
    SessionTitle,

    /// A turn is already running in this session.
    ///
    /// The one place a caller genuinely should branch: the UI keeps the text
    /// in the composer and re-enables it when the running turn finishes,
    /// rather than reporting a failure the user can do nothing about.
    #[error("this session is already working on something")]
    TurnBusy {
        /// The turn that holds the session. Logged, not shown.
        turn_id: String,
    },

    /// The approval being answered is unknown, already resolved, or expired.
    ///
    /// The one failure `approval_resolve` has, and it is deliberately not
    /// silent (PLAN 2.1): a dialog answering a request the runtime has already
    /// timed out must be told so it can re-sync through
    /// `approval_list_pending`, rather than closing on the belief that it
    /// allowed something.
    #[error("that approval request is no longer open")]
    ApprovalStale {
        /// The id that was answered. Logged, not shown.
        request_id: String,
    },

    /// `allow_session` was answered on a row that offers no session grant.
    ///
    /// Checked in Rust rather than trusted to the UI (PLAN 3.1). The request
    /// carries `session_grant_allowed: false` so the button is not drawn, but
    /// a WebView is not a place to enforce a policy rule, and this is the
    /// enforcement.
    #[error("`{tool}` cannot be allowed for a whole session")]
    GrantNotAllowed {
        /// The tool that was asked about.
        tool: String,
    },

    /// A settings value was refused, with the reason to show beside it.
    ///
    /// Carries the field so the panel can mark the input the user has to fix
    /// rather than raising a banner over the whole form. The reason is written
    /// for someone correcting a typo, which is why the validators in
    /// [`store::settings`](crate::store::settings) spell out what a good value
    /// looks like instead of quoting a parser.
    #[error("that {field} cannot be used: {reason}")]
    Settings {
        /// The field, named as the user sees it — "base URL", "model".
        field: &'static str,
        /// What is wrong with the value, and what a working one looks like.
        reason: String,
    },

    /// A value on the identity form cannot be used (PLAN 7.3, Phase 12).
    ///
    /// Same shape and same code as [`AppError::Settings`], deliberately: both
    /// are a form saying "this field is wrong, here is what a working value
    /// looks like", and the panel answers both the same way — mark the input,
    /// keep the text. A separate variant rather than a shared one only so the
    /// log says which form it was.
    #[error("that {field} cannot be used: {reason}")]
    Agent {
        /// The field, named as the user sees it — "name", "role", "tools".
        field: &'static str,
        /// What is wrong with the value, and what a working one looks like.
        reason: String,
    },

    /// A value on the memory form cannot be used (PLAN 7.3, Phase 14).
    ///
    /// Same shape and same code as [`AppError::Settings`] and
    /// [`AppError::Agent`], for the same reason: three forms saying "this field
    /// is wrong, here is what a working value looks like", answered the same
    /// way by whichever panel raised it. The memory *tools* surface the same
    /// message to the model instead, where it reads as a refusal that names
    /// the fix.
    #[error("that {field} cannot be used: {reason}")]
    Memory {
        /// The field, named as the user sees it — "text", "source".
        field: &'static str,
        /// What is wrong with the value, and what a working one looks like.
        reason: String,
    },

    /// No memory carries that id *for this identity*.
    ///
    /// Deliberately not distinguished from "it belongs to another identity":
    /// every memory accessor is scoped, and an error that said "that one is
    /// somebody else's" would confirm the existence of a record the caller has
    /// no business knowing about.
    #[error("no memory with id `{id}`")]
    MemoryNotFound {
        /// The id that was looked up.
        id: String,
    },

    /// No agent carries that id, so the UI is holding a stale list.
    ///
    /// Same shape and same reasoning as [`AppError::ProjectNotFound`]: refetch
    /// rather than branch on the code.
    #[error("that identity no longer exists")]
    AgentNotFound {
        /// The id that was looked up. Logged, not shown.
        id: String,
    },

    /// The built-in identity was asked to change.
    ///
    /// It is the single implicit assistant of Phases 5–11 written down, and it
    /// is what every session that named no identity resolves to. An editable
    /// default is one whose meaning drifts under the sessions already using it;
    /// a deletable one strands them. Carries no dedicated code for the reason
    /// [`AppError::SessionTitle`] does not: the UI does not draw the controls
    /// that reach it, and this is the enforcement behind that.
    #[error("the built-in identity cannot be {action}")]
    AgentBuiltin {
        /// What was attempted, for the message: "edited", "deleted".
        action: &'static str,
    },

    /// A routine's field cannot be used, and somebody has to change it.
    ///
    /// The same shape as [`AppError::Agent`] and for the same reason: the form
    /// that raised it can put the message beside the input that caused it. It
    /// carries the four refusals of the door as well as the shape checks —
    /// a skill nobody granted, one that will not parse, one nobody has run
    /// under watch, a standing approval the runbook never asked for — because
    /// to the person typing they are all "this field is wrong, and here is
    /// why" (PLAN 7.3, Phase 16).
    #[error("that {field} cannot be used: {reason}")]
    Routine {
        /// Which field: "name", "schedule", "skill", "grants", "budget",
        /// "run".
        field: &'static str,
        /// What is wrong with it, and what a working value looks like.
        reason: String,
    },

    /// No routine carries that id, so the UI is holding a stale list.
    #[error("that routine no longer exists")]
    RoutineNotFound {
        /// The id that was looked up. Logged, not shown.
        id: String,
    },

    /// A connector's field cannot be used, and somebody has to change it
    /// (PLAN 7.3, Phase 18).
    ///
    /// The same shape as [`AppError::Agent`] and [`AppError::Routine`], for the
    /// same reason: the form that raised it puts the message beside the input
    /// that caused it. Adding a connector is the one act in this application
    /// that names a program to run without a tool call in front of it, so the
    /// refusals here are deliberately about the *record* — an id that could
    /// collide with a tool name, a command nobody spelled — and never about
    /// whether the program is a good idea.
    #[error("that {field} cannot be used: {reason}")]
    Connector {
        /// Which field: "id", "name", "command", "args", "env".
        field: &'static str,
        /// What is wrong with it, and what a working value looks like.
        reason: String,
    },

    /// No connector carries that id, so the UI is holding a stale list.
    #[error("that connector no longer exists")]
    ConnectorNotFound {
        /// The id that was looked up. Logged, not shown.
        id: String,
    },

    /// The board named a run the log can no longer see (PLAN 7.3, Phase 17).
    ///
    /// Not an anomaly. A run is folded out of the tail of `audit.jsonl`, and a
    /// window that has been open while the agent worked is holding a list the
    /// log has since scrolled past. Refetching the board is the fix, which is
    /// what the UI does with it.
    #[error("that run is no longer in the log")]
    RunNotFound {
        /// The reference that was looked up. Logged, not shown.
        id: String,
    },

    /// An identity was deleted while routines still fire as it.
    ///
    /// Refused rather than cascaded, for the reason [`AppError::AgentInUse`]
    /// is: a routine is a standing instruction somebody wrote, and silently
    /// deleting one because an identity went away would take a clock off the
    /// wall without saying so. Repoint them or delete them first — which is
    /// also how you fire a role without losing what it was doing.
    #[error("this identity cannot be deleted while routines still fire as it ({count} do)")]
    AgentHasRoutines {
        /// How many routines name it.
        count: usize,
    },

    /// An identity was deleted while sessions still run as it.
    ///
    /// Refused rather than cascaded. Reassigning those sessions to another
    /// identity would rewrite what they were: a transcript full of `fs_write`
    /// calls made as a "scribe" is not a transcript a "reviewer" produced.
    /// Deleting them is the user's decision to take, so the count is named and
    /// the identity is kept.
    #[error("this identity cannot be deleted while sessions still run as it ({count} do)")]
    AgentInUse {
        /// How many sessions are bound to it.
        count: usize,
    },

    /// The OS credential store could not be used.
    ///
    /// Deliberately carries nothing. Every platform's failure text names
    /// services, files or bundle identifiers, which is diagnosis for a log and
    /// noise for a user; the actionable part — that the key has to go in
    /// `AEGIS_API_KEY` instead — is what the settings panel says on the
    /// strength of `keyring_available` being false.
    #[error("your system's credential store could not be used")]
    Keyring,

    /// The window asked to reveal a path that is not inside the open
    /// project's workspace (PLAN 7.10).
    ///
    /// Arbitrary paths from the WebView are refused rather than opened. The
    /// argument is echoed because it is what the window sent, not a location
    /// discovered on the machine.
    #[error("`{path}` is not inside this workspace")]
    RevealOutside {
        /// The path as the window sent it.
        path: String,
    },

    /// The window asked to reveal a path that could not be opened.
    ///
    /// Distinct from [`AppError::RevealOutside`]: this one never resolved to a
    /// location, or the location has gone. Opening it is not a containment
    /// miss, it is a path that is not there to show.
    #[error("`{path}` cannot be opened: {reason}")]
    RevealPath {
        /// The path as the window sent it, or as it resolved.
        path: String,
        /// Why it cannot be opened, in words a user can act on.
        reason: String,
    },

    /// A runtime invariant broke somewhere outside the agent and tool
    /// domains — a channel that closed, a resource that vanished mid-call.
    ///
    /// The text is written for a user; the diagnosis belongs in the log.
    #[error("{what}")]
    Internal {
        /// What went wrong, in one clause.
        what: &'static str,
    },

    /// Reading the audit log failed.
    ///
    /// Distinct from [`AppError::Store`] because the two are different
    /// reassurances to give: a project list that will not load is an
    /// inconvenience, while an audit log that will not read means the record
    /// of what the agent did is not available, and the user should be told
    /// which one they are looking at.
    #[error("could not {action} the audit log")]
    Audit {
        /// What was being attempted, for the message.
        action: &'static str,
        /// The underlying failure. Never rendered into the message.
        #[source]
        source: std::io::Error,
    },

    /// Reading or writing the on-disk store failed.
    ///
    /// The message stays deliberately vague: the underlying `io::Error` and
    /// the path are logged at the call site, where they help, rather than
    /// handed to the WebView, where they only describe the machine.
    #[error("could not {action} your projects")]
    Store {
        /// What was being attempted, for the message.
        action: &'static str,
        /// The underlying failure. Never rendered into the message.
        #[source]
        source: std::io::Error,
    },
}

impl AppError {
    /// The stable code for this failure.
    pub const fn code(&self) -> ErrorCode {
        match self {
            Self::WorkspacePath { .. } => ErrorCode::PathInvalid,
            Self::TurnBusy { .. } => ErrorCode::TurnBusy,
            Self::ApprovalStale { .. } => ErrorCode::ApprovalStale,
            Self::GrantNotAllowed { .. } => ErrorCode::GrantNotAllowed,
            Self::Settings { .. }
            | Self::Agent { .. }
            | Self::Memory { .. }
            | Self::Routine { .. }
            | Self::Connector { .. } => ErrorCode::InvalidSetting,
            Self::Keyring => ErrorCode::KeyringUnavailable,
            Self::RevealOutside { .. } => ErrorCode::PathOutsideWorkspace,
            Self::RevealPath { .. } => ErrorCode::PathInvalid,
            Self::WindowUnavailable { .. }
            | Self::Runtime(_)
            | Self::WorkspaceScaffold { .. }
            | Self::ProjectNotFound { .. }
            | Self::SessionNotFound { .. }
            | Self::SessionTitle
            | Self::AgentNotFound { .. }
            | Self::AgentBuiltin { .. }
            | Self::AgentInUse { .. }
            | Self::AgentHasRoutines { .. }
            | Self::RoutineNotFound { .. }
            | Self::ConnectorNotFound { .. }
            | Self::RunNotFound { .. }
            | Self::MemoryNotFound { .. }
            | Self::Internal { .. }
            | Self::Audit { .. }
            | Self::Store { .. } => ErrorCode::Internal,
        }
    }
}

/// The JSON shape the WebView receives when a command rejects.
///
/// Kept as a private mirror rather than derived on [`AppError`] directly, so
/// the wire format stays flat and stable however the Rust enum grows.
///
/// `field` is present only where there is one — a refused settings value, or a
/// refused value on the identity form. It is what lets a form mark the input
/// the user has to fix instead of raising a banner over the whole panel, and it
/// is omitted rather than null for every other failure, so nothing that does
/// not have a field has to say so.
#[derive(Serialize)]
struct AppErrorPayload<'a> {
    code: ErrorCode,
    message: &'a str,
    retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    field: Option<&'a str>,
}

impl AppError {
    /// The input this failure is about, when it is about one.
    const fn field(&self) -> Option<&'static str> {
        match self {
            Self::Settings { field, .. }
            | Self::Agent { field, .. }
            | Self::Memory { field, .. }
            | Self::Routine { field, .. }
            | Self::Connector { field, .. } => Some(field),
            _ => None,
        }
    }
}

impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let code = self.code();
        AppErrorPayload {
            code,
            message: &self.to_string(),
            retryable: code.retryable(),
            field: self.field(),
        }
        .serialize(serializer)
    }
}

/// Shorthand for a fallible runtime operation.
pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_to_the_documented_wire_shape() {
        let err = AppError::WindowUnavailable {
            label: "main".to_owned(),
        };
        let json = serde_json::to_value(&err).expect("AppError serializes");

        assert_eq!(json["code"], "E_INTERNAL");
        assert_eq!(json["message"], "window `main` is not available");
        assert_eq!(json["retryable"], false);
    }

    #[test]
    fn a_bad_workspace_names_itself_and_the_reason() {
        let err = AppError::WorkspacePath {
            path: r"C:\gone".to_owned(),
            reason: "the folder does not exist".to_owned(),
        };
        let json = serde_json::to_value(&err).expect("AppError serializes");

        assert_eq!(json["code"], "E_PATH_INVALID");
        assert_eq!(
            json["message"],
            r"`C:\gone` cannot be used as a workspace: the folder does not exist"
        );
    }

    #[test]
    fn revealing_a_path_outside_the_workspace_names_the_argument() {
        let err = AppError::RevealOutside {
            path: "../elsewhere".to_owned(),
        };
        let json = serde_json::to_value(&err).expect("AppError serializes");

        assert_eq!(json["code"], "E_PATH_OUTSIDE_WORKSPACE");
        assert_eq!(
            json["message"],
            "`../elsewhere` is not inside this workspace"
        );
        assert_eq!(json["retryable"], false);
    }

    #[test]
    fn store_failures_do_not_describe_the_machine() {
        let err = AppError::Store {
            action: "save",
            source: std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                r"C:\Users\someone\AppData\Roaming\Aegis\projects.json is locked",
            ),
        };
        let message = err.to_string();

        assert_eq!(message, "could not save your projects");
        assert!(
            !message.contains("AppData"),
            "the source is for the log, not the WebView"
        );
    }

    /// A refused settings value names the input it is about, so the panel can
    /// mark that field rather than raising a banner over the whole form.
    #[test]
    fn a_refused_setting_names_its_field() {
        let err = AppError::Settings {
            field: "base URL",
            reason: "it is not a URL".to_owned(),
        };
        let json = serde_json::to_value(&err).expect("AppError serializes");

        assert_eq!(json["code"], "E_INVALID_SETTING");
        assert_eq!(json["field"], "base URL");
        assert_eq!(
            json["message"],
            "that base URL cannot be used: it is not a URL"
        );
        assert_eq!(
            json["retryable"], false,
            "retyping the same value fails again"
        );
    }

    /// Every other failure omits the key entirely rather than sending a null
    /// the UI would have to test for.
    #[test]
    fn a_failure_about_no_particular_field_carries_none() {
        let json = serde_json::to_value(AppError::Keyring).expect("AppError serializes");

        assert_eq!(json["code"], "E_KEYRING_UNAVAILABLE");
        assert!(json.get("field").is_none(), "{json}");
        assert!(
            !json["message"]
                .as_str()
                .unwrap_or_default()
                .contains("keyring"),
            "the platform's own words belong in the log: {json}"
        );
    }

    #[test]
    fn codes_are_unique_and_prefixed() {
        const ALL: [ErrorCode; 18] = [
            ErrorCode::TurnBusy,
            ErrorCode::NoWorkspace,
            ErrorCode::PathOutsideWorkspace,
            ErrorCode::PathInvalid,
            ErrorCode::Denied,
            ErrorCode::GrantNotAllowed,
            ErrorCode::ApprovalStale,
            ErrorCode::Timeout,
            ErrorCode::ToolFailed,
            ErrorCode::ProviderHttp,
            ErrorCode::ProviderParse,
            ErrorCode::NoApiKey,
            ErrorCode::KeyringUnavailable,
            ErrorCode::Cancelled,
            ErrorCode::TooManyToolRounds,
            ErrorCode::ScreenPermission,
            ErrorCode::InvalidSetting,
            ErrorCode::Internal,
        ];

        let mut seen = std::collections::HashSet::new();
        for code in ALL {
            assert!(
                code.as_str().starts_with("E_"),
                "{code} lacks the E_ prefix"
            );
            assert!(seen.insert(code.as_str()), "{code} is declared twice");
        }
    }
}
