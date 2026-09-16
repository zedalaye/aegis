//! The error type every IPC command returns.
//!
//! Messages are written for a user and never describe the machine or carry a
//! secret. Codes are the stable contract the UI branches on (PLAN 4.4); messages
//! may be reworded.

use std::fmt;

use serde::{Serialize, Serializer};

/// Stable failure codes, shared by IPC results and the `ToolResult` envelopes
/// the model sees (PLAN 4.4). [`ErrorCode::Internal`] is never worth branching
/// on; [`ErrorCode::InvalidSetting`] is every form's "mark this field".
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
    /// The project's execution host cannot take the command (PLAN 7.12): it ran
    /// nowhere, and the fix is on the project, not the call.
    ExecHost,
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
    /// The turn repeated the same tool calls and was stopped as a loop.
    ToolLoop,
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
            Self::ExecHost => "E_EXEC_HOST",
            Self::ProviderHttp => "E_PROVIDER_HTTP",
            Self::ProviderParse => "E_PROVIDER_PARSE",
            Self::NoApiKey => "E_NO_API_KEY",
            Self::KeyringUnavailable => "E_KEYRING_UNAVAILABLE",
            Self::Cancelled => "E_CANCELLED",
            Self::TooManyToolRounds => "E_TOO_MANY_TOOL_ROUNDS",
            Self::ToolLoop => "E_TOOL_LOOP",
            Self::ScreenPermission => "E_SCREEN_PERMISSION",
            Self::InvalidSetting => "E_INVALID_SETTING",
            Self::Internal => "E_INTERNAL",
        }
    }

    /// Whether repeating the identical request could succeed; the UI offers a
    /// retry only then.
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
            | Self::ExecHost
            | Self::NoApiKey
            | Self::KeyringUnavailable
            | Self::Cancelled
            | Self::TooManyToolRounds
            | Self::ToolLoop
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

    /// A path offered as a workspace cannot be used. Echoed: the user chose it.
    #[error("`{path}` cannot be used as a workspace: {reason}")]
    WorkspacePath {
        /// The path as it was given.
        path: String,
        /// Why it was refused, in words a user can act on.
        reason: String,
    },

    /// Part of the shared-workspace convention could not be created (Phase 11).
    /// The path is relative to the workspace root.
    #[error("could not create `{path}` in the workspace: {reason}")]
    WorkspaceScaffold {
        /// The convention path, relative to the workspace root.
        path: String,
        /// Why it could not be created, in words a user can act on.
        reason: String,
    },

    /// No project carries that id: the UI's list is stale, refetch.
    #[error("that project no longer exists")]
    ProjectNotFound {
        /// The id that was looked up. Logged, not shown.
        id: String,
    },

    /// No session carries that id: the UI's list is stale, refetch.
    #[error("that session no longer exists")]
    SessionNotFound {
        /// The id that was looked up. Logged, not shown.
        id: String,
    },

    /// A rename arrived empty. The UI prevents it; this is the enforcement.
    #[error("a session needs a title")]
    SessionTitle,

    /// A turn is already running in this session; the UI keeps the composer
    /// text and waits.
    #[error("this session is already working on something")]
    TurnBusy {
        /// The turn that holds the session. Logged, not shown.
        turn_id: String,
    },

    /// The approval being answered is unknown, resolved or expired; the dialog
    /// re-syncs through `approval_list_pending` (PLAN 2.1).
    #[error("that approval request is no longer open")]
    ApprovalStale {
        /// The id that was answered. Logged, not shown.
        request_id: String,
    },

    /// `allow_session` on a row that offers no session grant, enforced here
    /// rather than trusted to the WebView (PLAN 3.1).
    #[error("`{tool}` cannot be allowed for a whole session")]
    GrantNotAllowed {
        /// The tool that was asked about.
        tool: String,
    },

    /// A settings value was refused; `field` lets the panel mark the input.
    #[error("that {field} cannot be used: {reason}")]
    Settings {
        /// The field, named as the user sees it — "base URL", "model".
        field: &'static str,
        /// What is wrong with the value, and what a working one looks like.
        reason: String,
    },

    /// A value on the identity form cannot be used (Phase 12). Same code as
    /// [`AppError::Settings`]; its own variant for the log.
    #[error("that {field} cannot be used: {reason}")]
    Agent {
        /// The field, named as the user sees it — "name", "role", "tools".
        field: &'static str,
        /// What is wrong with the value, and what a working one looks like.
        reason: String,
    },

    /// A value on the memory form cannot be used (Phase 14). The memory tools
    /// return the same message to the model.
    #[error("that {field} cannot be used: {reason}")]
    Memory {
        /// The field, named as the user sees it — "text", "source".
        field: &'static str,
        /// What is wrong with the value, and what a working one looks like.
        reason: String,
    },

    /// No memory carries that id *for this identity* — never "it is someone
    /// else's", which would confirm it exists.
    #[error("no memory with id `{id}`")]
    MemoryNotFound {
        /// The id that was looked up.
        id: String,
    },

    /// No identity carries that id: the UI's list is stale, refetch.
    #[error("that identity no longer exists")]
    AgentNotFound {
        /// The id that was looked up. Logged, not shown.
        id: String,
    },

    /// The built-in identity was asked to change. The UI draws no control for
    /// it; this is the enforcement.
    #[error("the built-in identity cannot be {action}")]
    AgentBuiltin {
        /// What was attempted, for the message: "edited", "deleted".
        action: &'static str,
    },

    /// A routine's field cannot be used (Phase 16), including the door's
    /// refusals: skill not granted, unparseable, never run under watch, or a
    /// standing approval the runbook does not declare.
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

    /// A connector's field cannot be used (Phase 18). Checks the record only —
    /// never whether the program is a good idea.
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

    /// The board named a run the audit tail has scrolled past (Phase 17):
    /// refetch the board.
    #[error("that run is no longer in the log")]
    RunNotFound {
        /// The reference that was looked up. Logged, not shown.
        id: String,
    },

    /// An identity was deleted while routines still fire as it. Refused, not
    /// cascaded: repoint or delete them first.
    #[error("this identity cannot be deleted while routines still fire as it ({count} do)")]
    AgentHasRoutines {
        /// How many routines name it.
        count: usize,
    },

    /// An identity was deleted while sessions still run as it. Refused, not
    /// reassigned: a transcript stays the record of who acted.
    #[error("this identity cannot be deleted while sessions still run as it ({count} do)")]
    AgentInUse {
        /// How many sessions are bound to it.
        count: usize,
    },

    /// The OS credential store could not be used. Carries nothing: platform
    /// text belongs in the log, and the panel suggests `AEGIS_API_KEY`.
    #[error("your system's credential store could not be used")]
    Keyring,

    /// The window named a path outside the open workspace (PLAN 7.10, 7.15),
    /// for reveal and the explorer alike
    /// ([`reveal::target`](crate::reveal::target)).
    #[error("`{path}` is not inside this workspace")]
    RevealOutside {
        /// The path as the window sent it.
        path: String,
    },

    /// A path inside the workspace that cannot be shown: missing, or the wrong
    /// kind (a folder to preview, text asked for as an image).
    #[error("`{path}` cannot be opened: {reason}")]
    RevealPath {
        /// The path as the window sent it, or as it resolved.
        path: String,
        /// Why it cannot be opened, in words a user can act on.
        reason: String,
    },

    /// A whole drop could not become a brief (PLAN 7.15): no usable
    /// `.aegis/briefs/`, or the drop expired. Per-file refusals are in the report.
    #[error("the drop could not become a brief: {reason}")]
    BriefImport {
        /// Why, in words somebody can act on.
        reason: String,
    },

    /// A roster proposal could not be applied (PLAN 7.14) — all or nothing,
    /// including when the file changed since it was shown. A banner, not a field.
    #[error("the roster was not applied: {reason}")]
    Roster {
        /// Why, in words somebody can act on.
        reason: String,
    },

    /// An execution host chosen in the picker cannot be used (PLAN 7.12),
    /// refused when chosen rather than on every later command.
    #[error("that execution host cannot be used: {reason}")]
    ExecHost {
        /// Why it cannot, in words somebody can act on.
        reason: String,
    },

    /// A runtime invariant broke outside the agent and tool domains. Diagnosis
    /// goes to the log.
    #[error("{what}")]
    Internal {
        /// What went wrong, in one clause.
        what: &'static str,
    },

    /// Reading the audit log failed — kept apart from [`AppError::Store`] so
    /// the user knows the record is what is unavailable.
    #[error("could not {action} the audit log")]
    Audit {
        /// What was being attempted, for the message.
        action: &'static str,
        /// The underlying failure. Never rendered into the message.
        #[source]
        source: std::io::Error,
    },

    /// Reading or writing the on-disk store failed. The path and `io::Error`
    /// are logged at the call site, not sent to the WebView.
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
            | Self::Roster { .. }
            | Self::Connector { .. } => ErrorCode::InvalidSetting,
            Self::Keyring => ErrorCode::KeyringUnavailable,
            Self::ExecHost { .. } => ErrorCode::ExecHost,
            Self::RevealOutside { .. } => ErrorCode::PathOutsideWorkspace,
            Self::RevealPath { .. } | Self::BriefImport { .. } => ErrorCode::PathInvalid,
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

/// The flat JSON shape the WebView receives when a command rejects. `field` is
/// omitted, not null, when a failure is about no form input.
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
        const ALL: [ErrorCode; 20] = [
            ErrorCode::TurnBusy,
            ErrorCode::NoWorkspace,
            ErrorCode::PathOutsideWorkspace,
            ErrorCode::PathInvalid,
            ErrorCode::Denied,
            ErrorCode::GrantNotAllowed,
            ErrorCode::ApprovalStale,
            ErrorCode::Timeout,
            ErrorCode::ToolFailed,
            ErrorCode::ExecHost,
            ErrorCode::ProviderHttp,
            ErrorCode::ProviderParse,
            ErrorCode::NoApiKey,
            ErrorCode::KeyringUnavailable,
            ErrorCode::Cancelled,
            ErrorCode::TooManyToolRounds,
            ErrorCode::ToolLoop,
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
