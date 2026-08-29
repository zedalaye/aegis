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
/// never a new vocabulary the UI has to learn. [`ErrorCode::Internal`] is the
/// one addition to the PLAN list: it is the catch-all for a runtime failure
/// that is not part of the agent/tool domain — a missing window, a broken
/// event channel — and it is never a code the UI should branch on.
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
            Self::WindowUnavailable { .. }
            | Self::Runtime(_)
            | Self::ProjectNotFound { .. }
            | Self::SessionNotFound { .. }
            | Self::SessionTitle
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
#[derive(Serialize)]
struct AppErrorPayload<'a> {
    code: ErrorCode,
    message: &'a str,
    retryable: bool,
}

impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let code = self.code();
        AppErrorPayload {
            code,
            message: &self.to_string(),
            retryable: code.retryable(),
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

    #[test]
    fn codes_are_unique_and_prefixed() {
        const ALL: [ErrorCode; 17] = [
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
