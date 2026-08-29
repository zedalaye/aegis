//! The approval gate.
//!
//! Every tool call the model makes passes through [`decide`] before anything
//! touches the machine. The answer is one of three things (PLAN 4.2, step 3):
//! run it, ask the user, or refuse outright. Phase 4 gives the tools something
//! to do with that answer; this phase is the decision itself, on purpose —
//! writing the tools first makes it tempting to scatter path checks inside
//! each one, and a check that lives in five places is a check that is missing
//! from one of them.
//!
//! The shape that prevents that is [`Decision`] carrying a [`ResolvedCall`]:
//! arguments are parsed once, here; paths are resolved once, here; and a tool
//! receives the already-resolved values rather than the strings the model
//! sent. A tool cannot re-resolve a path differently from the way policy
//! judged it, because it never sees the original.
//!
//! What this gate is *not* is stated as plainly in PLAN 3.3 and belongs
//! repeated here: there is no sandbox. Tools run as the user, with the user's
//! environment and privileges. The boundary is that the user reads the exact
//! path, program, arguments and working directory before anything mutating
//! runs — and that every call, approved or not, leaves an audit line.
//!
//! Layout: [`path`] resolves and contains, [`matrix`] holds the decision table
//! of PLAN 3, [`grants`] remembers what a session already approved, and this
//! module is the entry point that puts the three together.

pub mod grants;
pub mod matrix;
pub mod path;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::ErrorCode;

pub use grants::{Grant, GrantStore};

/// Tool names. One string per tool, shared by the registry, the decision table
/// and the audit log, so a tool's schema, its dispatch and its policy row all
/// key off the same constant (PLAN 4.1).
pub mod tool {
    /// List a directory.
    pub const FS_LIST: &str = "fs_list";
    /// Read a file.
    pub const FS_READ: &str = "fs_read";
    /// Write a file.
    pub const FS_WRITE: &str = "fs_write";
    /// Run a program.
    pub const SHELL_EXEC: &str = "shell_exec";
    /// Capture a display.
    pub const SCREEN_CAPTURE: &str = "screen_capture";
}

/// How alarming a call should look in the approval dialog.
///
/// Advisory only: the risk badge changes the wording and the colour, never
/// whether something is asked about. Nothing downstream branches on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    /// Reversible, and bounded by the workspace.
    Low,
    /// Mutating, or reaching past what the user was looking at.
    Medium,
    /// Outside the workspace, secret-shaped, or arbitrary code.
    High,
}

/// The structured half of an approval request: what the dialog draws.
///
/// One variant per tool, so the dialog renders a file path with a size or a
/// command line with its working directory, rather than a JSON blob the user
/// has to parse (PLAN 2.1, `ApprovalDetail`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApprovalDetail {
    /// Listing a directory.
    FsList {
        /// The resolved directory.
        path: String,
    },
    /// Reading a file.
    FsRead {
        /// The resolved file.
        path: String,
        /// Its size, or `None` when it does not exist yet.
        bytes: Option<u64>,
    },
    /// Writing a file.
    FsWrite {
        /// The resolved target.
        path: String,
        /// How much would be written.
        bytes: u64,
        /// Whether this overwrites something.
        exists: bool,
        /// The first few kilobytes of the content, for the diff pane.
        preview: Option<String>,
    },
    /// Running a program.
    Shell {
        /// The program as the model named it.
        program: String,
        /// Its arguments, unjoined.
        args: Vec<String>,
        /// The resolved working directory.
        cwd: String,
        /// A display-only rendering of the whole command.
        ///
        /// Never executed and never parsed: `shell_exec` spawns `program` with
        /// `args` directly, with no shell in between (PLAN 5.1). This string
        /// exists so a user can read one line instead of five fields.
        shell_line: String,
    },
    /// Capturing a display.
    Screen {
        /// Which display, named the way the dialog should say it.
        display: String,
        /// Physical width in pixels, `0` when the geometry is not known yet.
        width: u32,
        /// Physical height in pixels, `0` when the geometry is not known yet.
        height: u32,
    },
}

/// Everything policy knows about a call it wants the user to approve.
///
/// The ids, timestamps and expiry of PLAN's `ApprovalRequest` are added by the
/// approval registry in Phase 6; policy has no business inventing them. What
/// policy owns is the wording and the scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskRequest {
    /// Which tool asked.
    pub tool: &'static str,
    /// The badge.
    pub risk: Risk,
    /// The dialog's title: "Write file", "Run shell command".
    pub title: &'static str,
    /// One line naming the thing: `src/main.rs (2.4 KB, overwrite)`.
    pub summary: String,
    /// The structured detail the dialog renders.
    pub detail: ApprovalDetail,
    /// The grant an `allow_session` answer would create, when this row offers
    /// one.
    ///
    /// `None` is PLAN's `session_grant_allowed: false`, and it is more than a
    /// hint to the UI: because the grant to record *is* this value, an
    /// `allow_session` decision on a row that offers none has nothing to
    /// store, and Phase 6 rejects it with `E_GRANT_NOT_ALLOWED` rather than
    /// trusting the WebView to have hidden the button.
    pub grant: Option<Grant>,
    /// What an `allow_session` answer would cover, in words. Falls back to
    /// naming the single call when no grant is on offer.
    pub scope_label: String,
    /// Why policy is asking at all: "outside the workspace", "mutating".
    pub reason: String,
}

/// What policy decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Run it without asking. Audited with `decision: "auto"`.
    Auto {
        /// The call to execute, with paths already resolved.
        call: ResolvedCall,
        /// Why no prompt was needed. Goes to the audit log.
        reason: &'static str,
    },
    /// Ask the user first.
    Ask {
        /// The call to execute if they allow it.
        call: ResolvedCall,
        /// What to ask.
        request: Box<AskRequest>,
    },
    /// Refuse, without offering an approval (PLAN 3.2).
    ///
    /// Reserved for calls that could not be meaningfully approved: a path that
    /// does not resolve, a link that escapes the workspace while pretending
    /// not to, a program that is this application. The model sees an ordinary
    /// error envelope and can try something else — a denial is a result, not
    /// an exception (PLAN 4.3).
    Deny {
        /// The stable code for the envelope.
        code: ErrorCode,
        /// Why, in words, for the model and the transcript.
        reason: String,
    },
}

impl Decision {
    /// A refusal with a formatted reason.
    fn deny(code: ErrorCode, reason: impl Into<String>) -> Self {
        Self::Deny {
            code,
            reason: reason.into(),
        }
    }
}

/// A call whose paths have been resolved, ready to execute.
///
/// Produced only by [`decide`]. The tools in Phase 4 take this, never the raw
/// arguments, which is what makes "policy resolved it, the tool ran something
/// else" unrepresentable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedCall {
    /// `fs_list`, with the directory resolved.
    FsList {
        /// The directory to list.
        path: PathBuf,
        /// Caller's cap on entries; the tool applies the hard limit.
        max_entries: Option<u32>,
    },
    /// `fs_read`, with the file resolved.
    FsRead {
        /// The file to read.
        path: PathBuf,
        /// Byte offset to start at.
        offset: Option<u64>,
        /// Byte count to stop after.
        limit: Option<u64>,
    },
    /// `fs_write`, with the target resolved.
    FsWrite {
        /// The file to write.
        path: PathBuf,
        /// Exactly what to write.
        content: String,
        /// Whether missing parent directories may be created.
        create_dirs: bool,
    },
    /// `shell_exec`, with the working directory resolved.
    ShellExec {
        /// The program, as named. Phase 7 resolves it through PATH.
        program: String,
        /// Its arguments, passed as a vector — never through a shell.
        args: Vec<String>,
        /// The resolved working directory.
        cwd: PathBuf,
        /// Caller's deadline; the tool applies the hard ceiling.
        timeout_ms: Option<u64>,
    },
    /// `screen_capture`.
    ScreenCapture {
        /// Which display to capture.
        display: String,
    },
}

impl ResolvedCall {
    /// The tool this call belongs to.
    ///
    /// Read by dispatch and by the audit line, so a call cannot be executed
    /// under one name and logged under another.
    pub const fn tool(&self) -> &'static str {
        match self {
            Self::FsList { .. } => tool::FS_LIST,
            Self::FsRead { .. } => tool::FS_READ,
            Self::FsWrite { .. } => tool::FS_WRITE,
            Self::ShellExec { .. } => tool::SHELL_EXEC,
            Self::ScreenCapture { .. } => tool::SCREEN_CAPTURE,
        }
    }
}

/// A tool call as the model sent it, parsed but not yet resolved.
///
/// Public because it is the shape a caller can build directly in a test; in
/// the runtime it only ever comes from [`ToolCall::parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCall {
    /// `fs_list`.
    FsList {
        /// Directory, absolute or relative to the workspace.
        path: String,
        /// Caller's cap on entries.
        max_entries: Option<u32>,
    },
    /// `fs_read`.
    FsRead {
        /// File, absolute or relative to the workspace.
        path: String,
        /// Byte offset.
        offset: Option<u64>,
        /// Byte count.
        limit: Option<u64>,
    },
    /// `fs_write`.
    FsWrite {
        /// Target, absolute or relative to the workspace.
        path: String,
        /// Content to write.
        content: String,
        /// Whether to create missing parents.
        create_dirs: bool,
    },
    /// `shell_exec`.
    ShellExec {
        /// Program name or path.
        program: String,
        /// Arguments.
        args: Vec<String>,
        /// Working directory; the workspace root when absent.
        cwd: Option<String>,
        /// Deadline in milliseconds.
        timeout_ms: Option<u64>,
    },
    /// `screen_capture`.
    ScreenCapture {
        /// Display selector; `primary` when absent.
        display: Option<String>,
    },
}

impl ToolCall {
    /// The tool this call names.
    pub const fn tool(&self) -> &'static str {
        match self {
            Self::FsList { .. } => tool::FS_LIST,
            Self::FsRead { .. } => tool::FS_READ,
            Self::FsWrite { .. } => tool::FS_WRITE,
            Self::ShellExec { .. } => tool::SHELL_EXEC,
            Self::ScreenCapture { .. } => tool::SCREEN_CAPTURE,
        }
    }

    /// Parses the arguments a model produced.
    ///
    /// The error is written for the model, not for a user: it goes back as a
    /// `tool` message so the model can correct itself and the turn continues
    /// (PLAN 4.1). Unknown fields are ignored rather than refused — a model
    /// that adds a stray key should be answered, not stalled.
    pub fn parse(tool_name: &str, args: serde_json::Value) -> Result<Self, String> {
        fn convert<T: for<'de> Deserialize<'de>>(
            tool_name: &str,
            args: serde_json::Value,
        ) -> Result<T, String> {
            serde_json::from_value(args).map_err(|err| format!("{tool_name}: {err}"))
        }

        match tool_name {
            tool::FS_LIST => {
                let a: FsListArgs = convert(tool_name, args)?;
                Ok(Self::FsList {
                    path: a.path,
                    max_entries: a.max_entries,
                })
            }
            tool::FS_READ => {
                let a: FsReadArgs = convert(tool_name, args)?;
                Ok(Self::FsRead {
                    path: a.path,
                    offset: a.offset,
                    limit: a.limit,
                })
            }
            tool::FS_WRITE => {
                let a: FsWriteArgs = convert(tool_name, args)?;
                Ok(Self::FsWrite {
                    path: a.path,
                    content: a.content,
                    create_dirs: a.create_dirs,
                })
            }
            tool::SHELL_EXEC => {
                let a: ShellExecArgs = convert(tool_name, args)?;
                Ok(Self::ShellExec {
                    program: a.program,
                    args: a.args,
                    cwd: a.cwd,
                    timeout_ms: a.timeout_ms,
                })
            }
            tool::SCREEN_CAPTURE => {
                let a: ScreenCaptureArgs = convert(tool_name, args)?;
                Ok(Self::ScreenCapture { display: a.display })
            }
            other => Err(format!("unknown tool `{other}`")),
        }
    }
}

/// Wire shape of `fs_list` arguments (PLAN 4.1).
#[derive(Debug, Deserialize)]
struct FsListArgs {
    path: String,
    #[serde(default)]
    max_entries: Option<u32>,
}

/// Wire shape of `fs_read` arguments.
#[derive(Debug, Deserialize)]
struct FsReadArgs {
    path: String,
    #[serde(default)]
    offset: Option<u64>,
    #[serde(default)]
    limit: Option<u64>,
}

/// Wire shape of `fs_write` arguments.
#[derive(Debug, Deserialize)]
struct FsWriteArgs {
    path: String,
    content: String,
    #[serde(default)]
    create_dirs: bool,
}

/// Wire shape of `shell_exec` arguments.
#[derive(Debug, Deserialize)]
struct ShellExecArgs {
    program: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

/// Wire shape of `screen_capture` arguments.
#[derive(Debug, Deserialize)]
struct ScreenCaptureArgs {
    #[serde(default)]
    display: Option<String>,
}

/// The physical geometry of the display a capture would take.
///
/// Supplied by the caller rather than measured here, so policy stays a pure
/// function of its inputs and stays testable without a screen. Phase 9 fills
/// it from the capture backend; until then it is `None` and the dialog says
/// the size is not known rather than inventing one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenGeometry {
    /// How the display should be named to the user.
    pub display: String,
    /// Physical width in pixels.
    pub width: u32,
    /// Physical height in pixels.
    pub height: u32,
}

/// What a decision needs to know beyond the call itself.
#[derive(Debug, Clone, Copy)]
pub struct PolicyCtx<'a> {
    /// The session the call belongs to. Grants are keyed on it.
    pub session_id: &'a str,
    /// The session's workspace root, canonical and absolute.
    ///
    /// `None` means no project is bound, and PLAN 3.2 makes every tool call a
    /// hard denial in that state: with no root, "contained" has no meaning,
    /// and a decision table whose first column is undefined cannot be read.
    pub workspace: Option<&'a Path>,
    /// Live session grants.
    pub grants: &'a GrantStore,
    /// This application's own executable, when it is known.
    ///
    /// Used only to refuse `shell_exec` on ourselves. Passed in rather than
    /// looked up so the check is exercisable in a test.
    pub self_exe: Option<&'a Path>,
    /// The display geometry a capture would use, when it is known.
    pub screen: Option<&'a ScreenGeometry>,
}

impl<'a> PolicyCtx<'a> {
    /// The context of an ordinary session.
    pub fn new(session_id: &'a str, workspace: Option<&'a Path>, grants: &'a GrantStore) -> Self {
        Self {
            session_id,
            workspace,
            grants,
            self_exe: None,
            screen: None,
        }
    }

    /// Names this application's binary, so `shell_exec` can refuse to run it.
    #[must_use]
    pub const fn with_self_exe(mut self, exe: Option<&'a Path>) -> Self {
        self.self_exe = exe;
        self
    }

    /// Supplies the display geometry for capture approvals.
    #[must_use]
    pub const fn with_screen(mut self, screen: Option<&'a ScreenGeometry>) -> Self {
        self.screen = screen;
        self
    }
}

/// Decides what happens to one tool call.
///
/// The order is fixed and each step depends on the last: arguments have to
/// parse before a path can be resolved, a workspace has to exist before
/// containment means anything, and the decision table has to name a grant
/// before the store can be asked whether the session holds it.
pub fn decide(ctx: &PolicyCtx<'_>, tool_name: &str, args: serde_json::Value) -> Decision {
    let call = match ToolCall::parse(tool_name, args) {
        Ok(call) => call,
        Err(reason) => {
            tracing::debug!(tool = tool_name, "a tool call could not be parsed");
            return Decision::deny(ErrorCode::ToolFailed, reason);
        }
    };

    decide_call(ctx, call)
}

/// [`decide`], for a call that is already parsed.
pub fn decide_call(ctx: &PolicyCtx<'_>, call: ToolCall) -> Decision {
    let tool_name = call.tool();

    let Some(workspace) = ctx.workspace else {
        return Decision::deny(
            ErrorCode::NoWorkspace,
            "this session has no workspace, so no tool can run in it",
        );
    };

    let decision = matrix::decide(ctx, workspace, call);

    // A grant can only ever collapse an ask the table already raised. Rows
    // that offer no grant carry `None` here, so nothing in the store can
    // match them — which is how "never outside the workspace, never for a
    // .git/ write" is enforced without a second rule to keep in sync.
    let Decision::Ask { call, request } = decision else {
        return decision;
    };

    match &request.grant {
        Some(grant) if ctx.grants.holds(ctx.session_id, grant) => {
            tracing::debug!(tool = tool_name, "covered by a session grant");
            Decision::Auto {
                call,
                reason: "allowed for this session by an earlier approval",
            }
        }
        _ => Decision::Ask { call, request },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    #[test]
    fn unknown_tools_are_refused_in_terms_the_model_can_read() {
        let grants = GrantStore::new();
        let ctx = PolicyCtx::new("s1", None, &grants);

        match decide(&ctx, "rm_rf", json!({})) {
            Decision::Deny { code, reason } => {
                assert_eq!(code, ErrorCode::ToolFailed);
                assert!(reason.contains("rm_rf"), "{reason}");
            }
            other => panic!("expected a denial, got {other:?}"),
        }
    }

    #[test]
    fn missing_arguments_are_refused_before_anything_is_resolved() {
        let grants = GrantStore::new();
        let ctx = PolicyCtx::new("s1", None, &grants);

        match decide(&ctx, tool::FS_READ, json!({})) {
            Decision::Deny { code, reason } => {
                assert_eq!(code, ErrorCode::ToolFailed);
                assert!(reason.contains("path"), "{reason}");
            }
            other => panic!("expected a denial, got {other:?}"),
        }
    }

    #[test]
    fn a_stray_argument_does_not_stall_the_turn() {
        let call = ToolCall::parse(tool::FS_LIST, json!({ "path": ".", "depth": 3 }));
        assert!(call.is_ok(), "unknown keys are ignored, not refused");
    }

    #[test]
    fn without_a_workspace_nothing_runs() {
        let grants = GrantStore::new();
        let ctx = PolicyCtx::new("s1", None, &grants);

        match decide(&ctx, tool::SCREEN_CAPTURE, json!({})) {
            Decision::Deny { code, .. } => assert_eq!(code, ErrorCode::NoWorkspace),
            other => panic!("expected a denial, got {other:?}"),
        }
    }
}
