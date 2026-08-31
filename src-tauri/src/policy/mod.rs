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
//! From Phase 12 one question is asked before the table is read: may this
//! session's *identity* use this tool at all (PLAN 7.3)? That is not a
//! judgement about a path, it does not depend on a workspace, and it cannot be
//! approved past — so it is a check in [`decide_call`] rather than a row in the
//! matrix. A tool the identity does hold is then judged exactly as before: an
//! allow-list narrows what an identity could ever do, and never auto-allows a
//! call. Phase 13 adds the second allow-list beside it, for the same three
//! reasons: may this identity run this *skill*. Both refuse with `E_DENIED`
//! and neither opens a dialog, because a prompt offering to let an identity
//! exceed its own allow-list is a prompt that should not exist. Phase 15 adds
//! the one question in [`decide_call`] that is not about an identity at all —
//! whether this *run* is itself a delegated brief, since a specialist that
//! routed work would be a second Chief of Staff (`COS.md` *Roles*) — and it is
//! refused in the same place and for the same reason: it could not be
//! meaningfully approved.
//!
//! Layout: [`path`] resolves and contains, [`matrix`] holds the decision table
//! of PLAN 3, [`grants`] remembers what a session already approved, and this
//! module is the entry point that puts the three together.

pub mod grants;
pub mod matrix;
pub mod path;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::ErrorCode;
use crate::handoff;
use crate::store::memories;

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
    /// Load a runbook into this turn.
    pub const SKILL_RUN: &str = "skill_run";
    /// Record the result of the runbook that was loaded.
    pub const SKILL_RETURN: &str = "skill_return";
    /// Remember something as this identity.
    pub const MEMORY_WRITE: &str = "memory_write";
    /// Look through what this identity remembers.
    pub const MEMORY_SEARCH: &str = "memory_search";
    /// Hand briefs to other identities and wait for what comes back.
    pub const HANDOFF_DELEGATE: &str = "handoff_delegate";
    /// Close a delegated run with the report it was briefed for.
    pub const HANDOFF_RETURN: &str = "handoff_return";
}

/// How alarming a call should look in the approval dialog.
///
/// Advisory only: the risk badge changes the wording and the colour, never
/// whether something is asked about. Nothing downstream branches on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "bindings.ts")]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
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
        ///
        /// Exported as a `number` rather than a `bigint`: this crosses the IPC
        /// boundary as JSON, where it is already a double, and a `bigint` in
        /// the binding would be a type the value never has at runtime.
        #[ts(type = "number | null")]
        bytes: Option<u64>,
    },
    /// Writing a file.
    FsWrite {
        /// The resolved target.
        path: String,
        /// How much would be written. A `number` on the wire — see
        /// [`ApprovalDetail::FsRead::bytes`].
        #[ts(type = "number")]
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
        /// Physical width in pixels, `0` when the geometry is not known.
        ///
        /// A capture crate returns the physical framebuffer, so this is the
        /// size of the file that would be written. Both sizes are reported
        /// because on a scaled display they differ, and a dialog that showed
        /// only one of them would be describing a different picture from the
        /// one on the screen (PLAN 5.1).
        width: u32,
        /// Physical height in pixels, `0` when the geometry is not known.
        height: u32,
        /// Width in the display's own points — what the user calls its size.
        logical_width: u32,
        /// Height in the display's own points.
        logical_height: u32,
    },
    /// Remembering something (PLAN 7.3, Phase 14).
    ///
    /// The whole memory, not a preview of it: it is one sentence by
    /// construction, and a memory is the one mutating call where reading the
    /// entire thing costs the user less than reading a summary of it would.
    Memory {
        /// `preference`, `exception` or `convention`.
        ///
        /// Not `kind`, which is the enum's own discriminant tag on the wire.
        /// The two are different axes — *which tool asked* and *what sort of
        /// memory* — and one JSON object cannot spell them the same.
        memory_kind: String,
        /// Exactly what would be remembered.
        text: String,
        /// What it would rest on, when the model named something.
        source: Option<String>,
    },
    /// Handing work to other identities (PLAN 7.3, Phase 15).
    ///
    /// One row per brief, plus the reviewer's when one was asked for. The
    /// dialog draws the goals and the owners rather than the whole objects: a
    /// person deciding whether to spend four model runs is deciding *who is
    /// about to work on what*, and the constraints and the definition of done
    /// are in the brief file this names.
    Handoff {
        /// One row per brief, in the order they would go out.
        briefs: Vec<HandoffRow>,
        /// The identity that would review what comes back, when one was named.
        reviewer: Option<String>,
        /// Where the briefs would be filed, when the workspace has a `briefs/`.
        filed_in: Option<String>,
    },
}

/// One brief, as the approval dialog draws it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct HandoffRow {
    /// What is to be achieved.
    pub goal: String,
    /// The identity that would do it, as the model named it.
    pub owner: String,
    /// `high`, `normal` or `low`.
    pub priority: String,
    /// `status`, `artefact` or `question`.
    pub return_format: String,
    /// How many paths and links it starts from.
    pub inputs: u32,
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
    /// `skill_run`, with the name checked against the identity's allow-list.
    ///
    /// No path: a skill is named, not located, by the model. Where its runbook
    /// lives is a fact about the installation and the workspace, resolved by
    /// the tool from [`SkillCtx`](crate::skills::SkillCtx) — the same reason
    /// `screen_capture` does not carry the capture directory.
    SkillRun {
        /// The skill's name.
        name: String,
    },
    /// `skill_return`, with every artefact path resolved and contained.
    ///
    /// Boxed because it is by far the widest variant and every other call
    /// would otherwise pay for it in the size of the enum.
    SkillReturn {
        /// The return, as `COS.md` writes one.
        report: Box<handoff::Report>,
    },
    /// `memory_write`, with the kind already known to be one of the three.
    ///
    /// No identity: which identity is remembering is a fact about the turn,
    /// carried by [`ToolCtx`](crate::tools::ToolCtx), never by an argument the
    /// model writes. An identity cannot ask to remember something as somebody
    /// else because there is nowhere in this shape to say so.
    MemoryWrite {
        /// Which of the three kinds this is.
        kind: memories::MemoryKind,
        /// The memory itself, trimmed.
        text: String,
        /// What it rests on, when the model named something.
        source: Option<String>,
    },
    /// `memory_search`, scoped to the calling identity for the same reason.
    MemorySearch {
        /// The words that must all appear.
        query: String,
    },
    /// `handoff_delegate`, with every brief checked against `COS.md`'s shape.
    ///
    /// No owner resolution: which identity a name refers to is a fact about the
    /// agent registry, which policy cannot see and has no business consulting.
    /// The bus resolves it, and an owner nobody has heard of is a line on the
    /// board rather than a refused call — the other briefs still ran.
    ///
    /// Boxed for the reason `SkillReturn` is: it is the widest variant, and
    /// every other call would otherwise pay for it in the size of the enum.
    HandoffDelegate {
        /// Who gets what, and who checks it.
        plan: Box<handoff::Plan>,
    },
    /// `handoff_return`, with every artefact path resolved and contained.
    ///
    /// The same shape `SkillReturn` carries, because it is the same object: a
    /// run closes with a report whether a runbook framed it or a brief did.
    HandoffReturn {
        /// The return, as `COS.md` writes one.
        report: Box<handoff::Report>,
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
            Self::SkillRun { .. } => tool::SKILL_RUN,
            Self::SkillReturn { .. } => tool::SKILL_RETURN,
            Self::MemoryWrite { .. } => tool::MEMORY_WRITE,
            Self::MemorySearch { .. } => tool::MEMORY_SEARCH,
            Self::HandoffDelegate { .. } => tool::HANDOFF_DELEGATE,
            Self::HandoffReturn { .. } => tool::HANDOFF_RETURN,
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
    /// `skill_run`.
    SkillRun {
        /// The skill's name, already checked to be shaped like one.
        name: String,
    },
    /// `skill_return`.
    SkillReturn {
        /// The return, with its artefact paths still as the model wrote them.
        report: Box<handoff::Draft>,
    },
    /// `memory_write`.
    MemoryWrite {
        /// Which of the three kinds, already parsed — see
        /// [`ToolCall::parse`].
        kind: memories::MemoryKind,
        /// The memory as the model wrote it.
        text: String,
        /// The citation, when it gave one.
        source: Option<String>,
    },
    /// `memory_search`.
    MemorySearch {
        /// The words to look for; empty lists what is held.
        query: String,
    },
    /// `handoff_delegate`.
    HandoffDelegate {
        /// Who gets what, and who checks it, as the model wrote them.
        plan: Box<handoff::Plan>,
    },
    /// `handoff_return`.
    HandoffReturn {
        /// The return, with its artefact paths still as the model wrote them.
        report: Box<handoff::Draft>,
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
            Self::SkillRun { .. } => tool::SKILL_RUN,
            Self::SkillReturn { .. } => tool::SKILL_RETURN,
            Self::MemoryWrite { .. } => tool::MEMORY_WRITE,
            Self::MemorySearch { .. } => tool::MEMORY_SEARCH,
            Self::HandoffDelegate { .. } => tool::HANDOFF_DELEGATE,
            Self::HandoffReturn { .. } => tool::HANDOFF_RETURN,
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
            tool::SKILL_RUN => {
                let a: SkillRunArgs = convert(tool_name, args)?;
                let name = a.name.trim();
                // Checked here rather than by the tool, because the identity's
                // allow-list is matched against this string a moment later and
                // a name that could be `../../etc` would be a name the
                // allow-list and the filesystem disagree about.
                if !crate::skills::is_name(name) {
                    return Err(format!(
                        "`{name}` is not a skill name. They look like `inbox.triage`: lower-case \
                         letters, digits, `.`, `-` and `_`"
                    ));
                }
                Ok(Self::SkillRun {
                    name: name.to_owned(),
                })
            }
            tool::SKILL_RETURN => Ok(Self::SkillReturn {
                report: Box::new(draft(convert(tool_name, args)?)?),
            }),
            tool::HANDOFF_RETURN => Ok(Self::HandoffReturn {
                report: Box::new(draft(convert(tool_name, args)?)?),
            }),
            tool::HANDOFF_DELEGATE => {
                let a: HandoffDelegateArgs = convert(tool_name, args)?;
                let mut briefs = Vec::with_capacity(a.briefs.len());
                for one in a.briefs {
                    briefs.push(brief(one)?);
                }
                let review = match a.review {
                    Some(one) => Some(brief(one)?),
                    None => None,
                };
                Ok(Self::HandoffDelegate {
                    plan: Box::new(handoff::Plan { briefs, review }),
                })
            }
            tool::MEMORY_WRITE => {
                let a: MemoryWriteArgs = convert(tool_name, args)?;
                // Parsed here rather than by the tool, so that "which of the
                // three is this" is settled before the approval dialog has to
                // name it. A dialog cannot ask about a kind nobody has read.
                let Some(kind) = memories::MemoryKind::parse(&a.kind) else {
                    return Err(format!(
                        "`{}` is not a kind of memory. A memory is a `preference`, an \
                         `exception` or a `convention`; anything else is a file in the workspace \
                         or a skill",
                        a.kind.trim()
                    ));
                };
                Ok(Self::MemoryWrite {
                    kind,
                    text: a.text,
                    source: a.source,
                })
            }
            tool::MEMORY_SEARCH => {
                let a: MemorySearchArgs = convert(tool_name, args)?;
                Ok(Self::MemorySearch {
                    query: a.query.unwrap_or_default(),
                })
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

/// Wire shape of `skill_run` arguments.
#[derive(Debug, Deserialize)]
struct SkillRunArgs {
    name: String,
}

/// Wire shape of `memory_write` arguments.
///
/// `kind` arrives as a string rather than as the enum, for the reason
/// `skill_return`'s status does: an unrecognized one is then answered with the
/// three that work, instead of with whatever `serde` says about a variant name.
#[derive(Debug, Deserialize)]
struct MemoryWriteArgs {
    kind: String,
    text: String,
    #[serde(default)]
    source: Option<String>,
}

/// Wire shape of `memory_search` arguments.
#[derive(Debug, Deserialize)]
struct MemorySearchArgs {
    #[serde(default)]
    query: Option<String>,
}

/// Wire shape of a return, for `skill_return` and `handoff_return` alike
/// (`COS.md` *Handoff*).
///
/// One struct for both, because it is one object: a run closes with a report
/// whether a runbook framed it or a brief did, and two structs would be two
/// places for the shape to drift from `COS.md`.
///
/// `status` arrives as a string rather than as the enum so an unrecognized one
/// is answered with the three that work, instead of with whatever `serde`
/// says about a variant name.
#[derive(Debug, Deserialize)]
struct ReportArgs {
    status: String,
    summary: String,
    #[serde(default)]
    artefacts: Vec<String>,
    #[serde(default)]
    evidence: Vec<String>,
    #[serde(default)]
    open_questions: Vec<String>,
    #[serde(default)]
    next_owner: Option<String>,
}

/// Wire shape of `handoff_delegate` arguments (`COS.md` *Handoff*).
#[derive(Debug, Deserialize)]
struct HandoffDelegateArgs {
    briefs: Vec<BriefArgs>,
    #[serde(default)]
    review: Option<BriefArgs>,
}

/// Wire shape of one brief.
///
/// `priority` and `return_format` arrive as strings, for the reason a status
/// does: an unrecognized one is then answered with the words that work.
#[derive(Debug, Deserialize)]
struct BriefArgs {
    goal: String,
    owner: String,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    inputs: Vec<String>,
    #[serde(default)]
    constraints: Vec<String>,
    definition_of_done: String,
    #[serde(default)]
    approval_needed: Option<String>,
    #[serde(default)]
    return_format: Option<String>,
}

/// One return, from the arguments as they arrived.
///
/// Shared by `skill_return` and `handoff_return`, which is the point: the same
/// words mean the same thing whichever framed the run.
fn draft(a: ReportArgs) -> Result<handoff::Draft, String> {
    let status = match a.status.trim() {
        "done" => handoff::Status::Done,
        "blocked" => handoff::Status::Blocked,
        "needs_you" => handoff::Status::NeedsYou,
        other => {
            return Err(format!(
                "`{other}` is not a status. A run ends `done`, `blocked` or `needs_you`"
            ))
        }
    };

    Ok(handoff::Draft {
        status,
        summary: a.summary,
        artefacts: a.artefacts,
        evidence: a.evidence,
        open_questions: a.open_questions,
        next_owner: a.next_owner.unwrap_or_default(),
    })
}

/// One brief, from the arguments as they arrived.
///
/// The words are checked here, before the shape is: a model that wrote
/// `urgent` should be told the three that work rather than have `serde` refuse
/// the whole call for a reason about a variant name. The *content* — a goal
/// that is there, inputs that are paths — is [`handoff::check_brief`], applied
/// by the table below, because a refusal about content should not be
/// indistinguishable from one about spelling.
fn brief(a: BriefArgs) -> Result<handoff::Brief, String> {
    let priority = match a.priority.as_deref().map(str::trim) {
        None | Some("") | Some("normal") => handoff::Priority::Normal,
        Some("high") => handoff::Priority::High,
        Some("low") => handoff::Priority::Low,
        Some(other) => {
            return Err(format!(
                "`{other}` is not a priority. A brief is `high`, `normal` or `low`"
            ))
        }
    };

    let return_format = match a.return_format.as_deref().map(str::trim) {
        None | Some("") | Some("status") => handoff::ReturnFormat::Status,
        Some("artefact") => handoff::ReturnFormat::Artefact,
        Some("question") => handoff::ReturnFormat::Question,
        Some(other) => {
            return Err(format!(
                "`{other}` is not a return format. A brief asks for a `status`, an `artefact` or \
                 a `question`"
            ))
        }
    };

    Ok(handoff::Brief {
        goal: a.goal,
        owner: a.owner,
        priority,
        inputs: a.inputs,
        constraints: a.constraints,
        definition_of_done: a.definition_of_done,
        approval_needed: a.approval_needed.unwrap_or_default(),
        return_format,
    })
}

/// The geometry of the display a capture would take.
///
/// Supplied by the caller rather than measured here, so policy stays a pure
/// function of its inputs and stays testable without a screen — the turn loop
/// fills it from
/// [`screenshot::geometry`](crate::tools::screenshot::geometry). `None` is a
/// machine with no display the window server will describe, and the dialog
/// then says the size is not known rather than inventing one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenGeometry {
    /// How the display should be named to the user.
    pub display: String,
    /// Physical width in pixels — the size of the file a capture would write.
    pub width: u32,
    /// Physical height in pixels.
    pub height: u32,
    /// Width in the display's own points, at whatever scale it is set to.
    pub logical_width: u32,
    /// Height in the display's own points.
    pub logical_height: u32,
}

/// The identity a call is made under, as policy needs to see it
/// (PLAN 7.3, Phase 12).
///
/// Two fields rather than the whole [`Agent`](crate::store::Agent): policy has
/// no business with an identity's instructions or its provider binding, and
/// keeping the borrow this narrow is what lets [`PolicyCtx`] stay `Copy`. The
/// name is here because it goes in the refusal — "the Reviewer identity is not
/// allowed to use `fs_write`" is an answer the model can act on, and "denied"
/// is not.
#[derive(Debug, Clone, Copy)]
pub struct Identity<'a> {
    /// How the identity is named to the user and to the model.
    pub name: &'a str,
    /// The tools it may call.
    pub tools: &'a [String],
    /// The skills it may run (PLAN 7.3, Phase 13).
    ///
    /// A second allow-list rather than a wider first one, because they gate
    /// different things: a tool is a verb on the machine, a skill is a runbook
    /// that sequences those verbs. Holding a skill never adds a tool — the
    /// runner refuses a run whose declared tools are not all in the list above
    /// — and holding every tool never adds a skill.
    pub skills: &'a [String],
}

impl Identity<'_> {
    /// Whether this identity may call `tool`.
    fn allows(&self, tool: &str) -> bool {
        self.tools.iter().any(|granted| granted == tool)
    }

    /// Whether this identity may run `skill`.
    fn allows_skill(&self, skill: &str) -> bool {
        self.skills.iter().any(|granted| granted == skill)
    }
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
    /// Whether this turn is itself a delegated run (PLAN 7.3, Phase 15).
    ///
    /// True inside a specialist working on a brief, false in a session a person
    /// is typing into. It gates exactly one thing — `handoff_delegate` — and it
    /// is a fact about the *run* rather than about the identity, which is why
    /// it is here rather than on [`Identity`]: the same reviewer identity may be
    /// a CoS in one session and a specialist in the next, and its tool list does
    /// not change between them.
    pub delegated: bool,
    /// The identity the call is made under, and the tools it holds.
    ///
    /// `None` is "no identity is bound to this decision", which means every
    /// registered tool — the behaviour of Phases 4–11, and what a test that has
    /// no opinion about identities gets. The turn loop always names one: it
    /// resolves the session's identity before the first round and passes it
    /// here, so the runtime never takes this branch.
    pub identity: Option<Identity<'a>>,
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
            delegated: false,
            identity: None,
        }
    }

    /// Marks this as a call made inside a delegated run.
    #[must_use]
    pub const fn delegated(mut self) -> Self {
        self.delegated = true;
        self
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

    /// Names the identity the call is made under, and the tools it holds.
    #[must_use]
    pub const fn with_identity(mut self, identity: Identity<'a>) -> Self {
        self.identity = Some(identity);
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

    // Before the workspace, because it does not depend on one: an identity that
    // holds no `fs_write` holds none whether or not a folder is mounted, and
    // "you may not do this" is a truer answer than "there is nowhere to do it".
    //
    // The model was never shown this tool's schema
    // ([`schemas_for`](crate::tools::schemas_for)), so reaching here means the
    // call came from a transcript written under a wider grant, or the model
    // invented the name. Both are refused the same way, and both are refused
    // *here* rather than in the registry: a second enforcement point is a
    // second rule to keep in step with this one.
    if let Some(identity) = ctx.identity {
        if !identity.allows(tool_name) {
            tracing::info!(
                tool = tool_name,
                identity = identity.name,
                "a tool call outside the identity's allow-list"
            );
            // The name is quoted and carries no trailing noun, so an identity
            // whose name is already a noun phrase does not read as "the
            // Unknown identity identity".
            return Decision::deny(
                ErrorCode::Denied,
                format!("`{}` is not allowed to use `{tool_name}`", identity.name),
            );
        }

        // The second allow-list, in the same place and for the same reasons
        // (PLAN 7.3, Phase 13). It does not depend on a workspace — a runbook
        // in the library is granted or not whether or not a folder is mounted
        // — and it cannot be approved past, because a dialog offering to let
        // an identity run a skill it was not granted is the allow-list asking
        // to be overruled. The name was never in the identity's catalog, so
        // reaching here means an invented name or one out of an older
        // transcript.
        if let ToolCall::SkillRun { name } = &call {
            if !identity.allows_skill(name) {
                tracing::info!(
                    skill = %name,
                    identity = identity.name,
                    "a skill outside the identity's allow-list"
                );
                return Decision::deny(
                    ErrorCode::Denied,
                    format!("`{}` is not allowed to run `{name}`", identity.name),
                );
            }
        }
    }

    // Depth is one, and it is enforced here rather than left to the tool
    // because a dialog asking a person to approve a call that is going to be
    // refused anyway is a dialog that teaches them to click through
    // (PLAN 7.3, Phase 15; `COS.md` *Roles* — there are three, and a
    // specialist that routes work is a second Chief of Staff). The model was
    // not offered the schema either; reaching here means a name out of an
    // older transcript, or an invented one.
    if ctx.delegated && matches!(call, ToolCall::HandoffDelegate { .. }) {
        tracing::info!("a delegated run tried to delegate");
        return Decision::deny(
            ErrorCode::Denied,
            "you are working on a brief, and a brief is not re-delegated: one Chief of Staff \
             routes, specialists do the work. If this needs someone else, return `blocked` and \
             say who and why",
        );
    }

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

    /// The Phase 12 exit condition at the gate: an identity cannot use a tool
    /// it was not granted, even when it asks for one directly.
    #[test]
    fn a_tool_outside_the_identitys_allow_list_is_refused_by_name() {
        let grants = GrantStore::new();
        let workspace = std::env::current_dir().expect("a workspace to measure against");
        let allowed = vec![tool::FS_READ.to_owned(), tool::FS_LIST.to_owned()];
        let ctx = PolicyCtx::new("s1", Some(&workspace), &grants).with_identity(Identity {
            name: "Reviewer",
            tools: &allowed,
            skills: &[],
        });

        match decide(
            &ctx,
            tool::FS_WRITE,
            json!({ "path": "a.txt", "content": "x" }),
        ) {
            Decision::Deny { code, reason } => {
                // `E_DENIED` rather than a code of its own: the system message
                // already tells the model what to do with a denial, and a
                // second vocabulary for "policy said no" is one more thing for
                // it to get wrong.
                assert_eq!(code, ErrorCode::Denied);
                assert!(reason.contains("Reviewer"), "{reason}");
                assert!(reason.contains(tool::FS_WRITE), "{reason}");
            }
            other => panic!("expected a denial, got {other:?}"),
        }

        // And the tools it does hold are judged exactly as before.
        assert!(matches!(
            decide(&ctx, tool::FS_LIST, json!({ "path": "." })),
            Decision::Auto { .. }
        ));
    }

    /// The refusal is about the identity, not about the workspace: it holds
    /// when there is no folder to act in either, and it is the answer given.
    #[test]
    fn the_allow_list_is_checked_before_the_workspace_is() {
        let grants = GrantStore::new();
        let ctx = PolicyCtx::new("s1", None, &grants).with_identity(Identity {
            name: "Scribe",
            tools: &[],
            skills: &[],
        });

        match decide(&ctx, tool::FS_READ, json!({ "path": "a.txt" })) {
            Decision::Deny { code, reason } => {
                assert_eq!(code, ErrorCode::Denied);
                assert!(reason.contains("Scribe"), "{reason}");
            }
            other => panic!("expected a denial, got {other:?}"),
        }
    }

    /// The Phase 13 half of the same gate: a skill the identity was not
    /// granted is refused before the runbook is even located, with no approval
    /// offered — and holding every tool does not grant a skill.
    #[test]
    fn a_skill_outside_the_identitys_allow_list_is_refused_by_name() {
        let grants = GrantStore::new();
        let workspace = std::env::current_dir().expect("a workspace to measure against");
        let every: Vec<String> = crate::tools::names()
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        let held = vec!["inbox.triage".to_owned()];
        let ctx = PolicyCtx::new("s1", Some(&workspace), &grants).with_identity(Identity {
            name: "Triager",
            tools: &every,
            skills: &held,
        });

        match decide(&ctx, tool::SKILL_RUN, json!({ "name": "deploy.draft" })) {
            Decision::Deny { code, reason } => {
                assert_eq!(code, ErrorCode::Denied);
                assert!(reason.contains("Triager"), "{reason}");
                assert!(reason.contains("deploy.draft"), "{reason}");
            }
            other => panic!("expected a denial, got {other:?}"),
        }

        // The one it holds is auto-allowed: loading a runbook it was granted
        // is not a question to put to anyone.
        assert!(matches!(
            decide(&ctx, tool::SKILL_RUN, json!({ "name": "inbox.triage" })),
            Decision::Auto { .. }
        ));
    }

    /// A name that is not shaped like one never reaches the allow-list, so
    /// "granted" and "on the filesystem" cannot be made to disagree.
    #[test]
    fn a_skill_name_that_is_a_path_is_refused_at_the_door() {
        let call = ToolCall::parse(tool::SKILL_RUN, json!({ "name": "../../etc/passwd" }));
        let reason = call.expect_err("refused");
        assert!(reason.contains("inbox.triage"), "{reason}");
    }

    /// A decision with no identity behind it is the pre-Phase-12 one. Every
    /// policy test written before identities existed relies on this.
    #[test]
    fn a_decision_with_no_identity_gates_on_the_matrix_alone() {
        let grants = GrantStore::new();
        let workspace = std::env::current_dir().expect("a workspace to measure against");
        let ctx = PolicyCtx::new("s1", Some(&workspace), &grants);

        assert!(ctx.identity.is_none());
        assert!(matches!(
            decide(&ctx, tool::FS_LIST, json!({ "path": "." })),
            Decision::Auto { .. }
        ));
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
