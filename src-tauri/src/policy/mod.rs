//! The approval gate (PLAN 3).
//!
//! Every tool call passes through [`decide`] before anything touches the
//! machine, and gets one of three answers: run it, ask, or refuse. Arguments
//! are parsed and paths resolved once, here; a tool receives a
//! [`ResolvedCall`] and never the model's strings, so it cannot act on a path
//! other than the one policy judged.
//!
//! Before the table, [`decide_call`] refuses what no dialog could approve: a
//! tool or skill outside the identity's allow-lists, and delegation from a
//! delegated run. There is no sandbox (PLAN 3.3): the boundary is a person
//! reading the exact call, and every call is audited.
//!
//! [`path`] resolves and contains, [`matrix`] is the table, [`grants`] holds
//! what a session already approved.

pub mod grants;
pub mod matrix;
pub mod path;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::agent::decision::eval::EvalDoc;
use crate::agent::decision::{self, Question};
use crate::error::ErrorCode;
use crate::exec_host::{ExecHost, ExecTarget};
use crate::handoff;
use crate::mcp;
use crate::store::connectors;
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
    /// Run a signed project eval (PLAN 7.18).
    pub const JEV_EVAL: &str = "jev_eval";
    /// Ask the decision model questions the harness does not own yet.
    pub const JEV_ASK: &str = "jev_ask";
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

/// What the approval dialog draws: one variant per tool, so the user reads a
/// path or a command line rather than JSON (PLAN 2.1).
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
        /// Its size, or `None` when it does not exist yet. A `number` in the
        /// binding: over JSON it is never a `bigint`.
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
        /// The skill this write would make live, when it applies a proposal
        /// (PLAN 7.13).
        applies: Option<String>,
    },
    /// Running a program.
    Shell {
        /// The program as the model named it.
        program: String,
        /// Its arguments, unjoined.
        args: Vec<String>,
        /// The resolved working directory.
        cwd: String,
        /// The whole command on one line, for reading only: never executed or
        /// parsed (PLAN 5.1).
        shell_line: String,
        /// The distribution and its working directory, when the project has
        /// an execution host (PLAN 7.12). `cwd` is then still the folder
        /// containment was judged against, but not where the command starts,
        /// so the dialog shows both.
        host: Option<ExecTarget>,
    },
    /// Capturing a display.
    Screen {
        /// Which display, named the way the dialog should say it.
        display: String,
        /// Physical width in pixels — the size of the file — or `0` when
        /// unknown. The logical size sits beside it because the two differ on
        /// a scaled display (PLAN 5.1).
        width: u32,
        /// Physical height in pixels, `0` when the geometry is not known.
        height: u32,
        /// Width in the display's own points — what the user calls its size.
        logical_width: u32,
        /// Height in the display's own points.
        logical_height: u32,
    },
    /// Remembering something (PLAN 7.3, Phase 14): the whole sentence, not a
    /// preview.
    Memory {
        /// `preference`, `exception` or `convention`. Not `kind`, which is
        /// this enum's tag on the wire.
        memory_kind: String,
        /// Exactly what would be remembered.
        text: String,
        /// What it would rest on, when the model named something.
        source: Option<String>,
    },
    /// Handing work to other identities (PLAN 7.3, Phase 15): who works on
    /// what. The full briefs are in the files `filed_in` names.
    Handoff {
        /// One row per brief, in the order they would go out.
        briefs: Vec<HandoffRow>,
        /// The identity that would review what comes back, when one was named.
        reviewer: Option<String>,
        /// Where the briefs would be filed, when the workspace has a `.aegis/briefs/`.
        filed_in: Option<String>,
    },
    /// Calling a connector's tool (PLAN 7.3, Phase 18).
    ///
    /// The one detail that cannot say what would happen: the runtime knows the
    /// tool's name, the server's description and the model's arguments, and
    /// nothing else, so the dialog shows exactly that, attributed.
    Connector {
        /// The connector's id — the part before the `__`.
        connector: String,
        /// The connector's name, as the person who installed it wrote it.
        connector_name: String,
        /// The tool's own name, as the server spells it.
        tool: String,
        /// What the server says the tool does. The server's words, not ours.
        description: String,
        /// Whether the server claims the tool only reads. A claim, attributed.
        read_only_hint: bool,
        /// The arguments the model wrote, as indented JSON.
        arguments: String,
    },
    /// Running a signed project eval (PLAN 7.18).
    JevEval {
        /// The eval's name.
        name: String,
        /// `key: path` for every file that would be sent.
        inputs: Vec<String>,
        /// The question ids the file holds.
        questions: Vec<String>,
    },
    /// Sending model-written questions to TypeSafe (PLAN 7.18).
    JevAsk {
        /// The model the request would name, when a client is configured.
        model: Option<String>,
        /// How many questions.
        question_count: u32,
        /// One line per question.
        questions: Vec<String>,
        /// The state, indented and capped.
        state_preview: String,
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

/// What policy knows about a call it wants approved: the wording and the
/// scope. Ids and expiry are added by the approval registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskRequest {
    /// Which tool asked. Owned, because connectors name their own tools.
    pub tool: String,
    /// The badge.
    pub risk: Risk,
    /// The dialog's title: "Write file", "Run shell command".
    pub title: &'static str,
    /// One line naming the thing: `src/main.rs (2.4 KB, overwrite)`.
    pub summary: String,
    /// The structured detail the dialog renders.
    pub detail: ApprovalDetail,
    /// The grant an `allow_session` answer would create. With `None` there is
    /// nothing to record, and such an answer is rejected with
    /// `E_GRANT_NOT_ALLOWED` rather than trusting the UI to hide the button.
    pub grant: Option<Grant>,
    /// What an `allow_session` answer would cover, in words. Falls back to
    /// naming the single call when no grant is on offer.
    pub scope_label: String,
    /// Why policy is asking at all: "outside the workspace", "mutating".
    pub reason: String,
}

/// What policy decided. Not `Eq`: a connector call carries JSON.
#[derive(Debug, Clone, PartialEq)]
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
    /// Refuse without offering an approval (PLAN 3.2): calls nobody could
    /// meaningfully approve. The model gets an ordinary error envelope.
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

/// A call with its paths resolved, ready to execute.
///
/// Produced only by [`decide`], so a tool cannot run something other than what
/// policy judged. A connector call is the exception the type admits: its
/// arguments cannot be resolved, so it carries what the model wrote — which is
/// what the dialog showed.
#[derive(Debug, Clone, PartialEq)]
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
        /// The program, as named. Phase 7 resolves it through PATH — this
        /// process' PATH, or the distribution's when there is a `host`.
        program: String,
        /// Its arguments, passed as a vector — never through a shell.
        args: Vec<String>,
        /// The resolved working directory, on this computer.
        cwd: PathBuf,
        /// Where the command lands, when it is not this process (PLAN 7.12).
        /// Resolved here so the tool runs where the dialog said. Boxed to keep
        /// the enum small.
        host: Option<Box<ExecTarget>>,
        /// Caller's deadline; the tool applies the hard ceiling.
        timeout_ms: Option<u64>,
    },
    /// `screen_capture`.
    ScreenCapture {
        /// Which display to capture.
        display: String,
    },
    /// `skill_run`, with the name checked against the identity's allow-list.
    /// The tool locates the runbook ([`SkillCtx`](crate::skills::SkillCtx)).
    SkillRun {
        /// The skill's name.
        name: String,
    },
    /// `skill_return`, with every artefact path resolved and contained. Boxed:
    /// it is the widest variant.
    SkillReturn {
        /// The return, as `COS.md` writes one.
        report: Box<handoff::Report>,
    },
    /// `memory_write`. No identity field: who remembers comes from the turn
    /// ([`ToolCtx`](crate::tools::ToolCtx)), never from the model.
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
    /// `handoff_delegate`, with every brief checked against `COS.md` *Handoff*.
    /// Owners are resolved by the bus: an unknown one is a line on the board,
    /// not a refused call.
    HandoffDelegate {
        /// Who gets what, and who checks it.
        plan: Box<handoff::Plan>,
    },
    /// `handoff_return`: the same report as `SkillReturn`, artefacts contained.
    HandoffReturn {
        /// The return, as `COS.md` writes one.
        report: Box<handoff::Report>,
    },
    /// A tool of an external connector (PLAN 7.3, Phase 18), identified by the
    /// name the model used (`git__status`) — the same string the allow-list,
    /// the grant and the audit line use.
    Connector {
        /// The full name, `<connector>__<tool>`.
        name: String,
        /// The arguments as the model wrote them.
        args: serde_json::Value,
    },
    /// `jev_eval`: the eval as it was loaded when the call was judged, and
    /// its input files resolved and contained.
    JevEval {
        /// The signed eval. What runs is what the dialog described.
        eval: Box<EvalDoc>,
        /// State key → resolved file.
        inputs: Vec<(String, PathBuf)>,
    },
    /// `jev_ask`, with the state and questions checked.
    JevAsk {
        /// What the questions judge.
        state: serde_json::Value,
        /// The questions.
        questions: Vec<Question>,
    },
}

impl ResolvedCall {
    /// The tool this call belongs to, read by dispatch and by the audit line.
    pub fn tool(&self) -> &str {
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
            Self::Connector { name, .. } => name,
            Self::JevEval { .. } => tool::JEV_EVAL,
            Self::JevAsk { .. } => tool::JEV_ASK,
        }
    }
}

/// A tool call as the model sent it, parsed but not yet resolved.
///
/// Public because it is the shape a caller can build directly in a test; in
/// the runtime it only ever comes from [`ToolCall::parse`].
#[derive(Debug, Clone, PartialEq)]
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
    /// A tool of an external connector, recognized by the shape of its name.
    /// Whether anything answers to it is the table's question.
    Connector {
        /// The full name, as the model wrote it.
        name: String,
        /// The arguments, unread: their schema belongs to the server.
        args: serde_json::Value,
    },
    /// `jev_eval`.
    JevEval {
        /// The eval's name.
        name: String,
        /// Paths overriding the file's declared inputs, by key.
        inputs: BTreeMap<String, String>,
    },
    /// `jev_ask`.
    JevAsk {
        /// What the questions judge.
        state: serde_json::Value,
        /// The questions, parsed.
        questions: Vec<Question>,
    },
}

impl ToolCall {
    /// The tool this call names.
    pub fn tool(&self) -> &str {
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
            Self::Connector { name, .. } => name,
            Self::JevEval { .. } => tool::JEV_EVAL,
            Self::JevAsk { .. } => tool::JEV_ASK,
        }
    }

    /// Parses the arguments a model produced. Errors are written for the model,
    /// which gets them back and can correct itself (PLAN 4.1). Unknown fields
    /// are ignored.
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
                // Checked here because the allow-list matches this string next,
                // and a name like `../../etc` must never reach it.
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
                // Parsed here so the approval dialog can name the kind.
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
            // Before the connector arm, which would otherwise read these names.
            tool::JEV_EVAL => {
                let a: JevEvalArgs = convert(tool_name, args)?;
                let name = a.name.trim();
                if !crate::skills::is_name(name) {
                    return Err(format!(
                        "`{name}` is not an eval name. They look like `inbox.classify`"
                    ));
                }
                Ok(Self::JevEval {
                    name: name.to_owned(),
                    inputs: a.inputs,
                })
            }
            tool::JEV_ASK => {
                let a: JevAskArgs = convert(tool_name, args)?;
                decision::check_state(&a.state)?;
                Ok(Self::JevAsk {
                    state: a.state,
                    questions: decision::parse_questions(a.questions)?,
                })
            }
            // Shaped like a connector tool. Its arguments follow the server's
            // schema, so they are only checked to be an object (`tools/call`).
            other => match connectors::split_tool_name(other) {
                Some(_) if args.is_object() || args.is_null() => Ok(Self::Connector {
                    name: other.to_owned(),
                    args: if args.is_null() {
                        serde_json::json!({})
                    } else {
                        args
                    },
                }),
                Some(_) => Err(format!(
                    "`{other}` takes an object of arguments, and that was not one"
                )),
                None => Err(format!("unknown tool `{other}`")),
            },
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

/// Wire shape of `memory_write` arguments. `kind` is a string so an unknown one
/// is answered with the three that work.
#[derive(Debug, Deserialize)]
struct MemoryWriteArgs {
    kind: String,
    text: String,
    #[serde(default)]
    source: Option<String>,
}

/// Wire shape of `jev_eval` arguments.
#[derive(Debug, Deserialize)]
struct JevEvalArgs {
    name: String,
    #[serde(default)]
    inputs: BTreeMap<String, String>,
}

/// Wire shape of `jev_ask` arguments.
#[derive(Debug, Deserialize)]
struct JevAskArgs {
    #[serde(default)]
    state: serde_json::Value,
    questions: Vec<decision::QuestionDraft>,
}

/// Wire shape of `memory_search` arguments.
#[derive(Debug, Deserialize)]
struct MemorySearchArgs {
    #[serde(default)]
    query: Option<String>,
}

/// Wire shape of a return, shared by `skill_return` and `handoff_return`
/// (`COS.md` *Handoff*). `status` is a string so an unknown one is answered with
/// the three that work.
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

/// One return, from its arguments. Shared by both return tools.
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

/// One brief, from its arguments. Only the words are checked here; the content
/// is [`handoff::check_brief`], so a refusal about content never looks like one
/// about spelling.
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

/// The geometry of the display a capture would take, supplied by the caller
/// ([`screenshot::geometry`](crate::tools::screenshot::geometry)) so policy
/// stays pure.
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

/// The identity a call is made under (PLAN 7.3, Phase 12): its name, for the
/// refusal, and its allow-lists. Kept narrow so [`PolicyCtx`] stays `Copy`.
#[derive(Debug, Clone, Copy)]
pub struct Identity<'a> {
    /// How the identity is named to the user and to the model.
    pub name: &'a str,
    /// The tools it may call.
    pub tools: &'a [String],
    /// The skills it may run (Phase 13). Holding a skill never adds a tool, and
    /// holding every tool never adds a skill.
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
    /// The session's canonical workspace root. `None` refuses every call
    /// (PLAN 3.2).
    pub workspace: Option<&'a Path>,
    /// Live session grants.
    pub grants: &'a GrantStore,
    /// This application's own executable, so `shell_exec` can refuse to run it.
    pub self_exe: Option<&'a Path>,
    /// The display geometry a capture would use, when it is known.
    pub screen: Option<&'a ScreenGeometry>,
    /// Whether this turn is a delegated run (Phase 15). A fact about the run,
    /// not the identity.
    pub delegated: bool,
    /// Whether nobody can answer a dialog: a routine's run (Phase 16). Every
    /// ask becomes a refusal; what the run may do is what was signed on the
    /// routine, arriving as ordinary session grants.
    pub unattended: bool,
    /// The connector catalog, snapshotted once per turn (Phase 18) so the tools
    /// offered and the tools judged are one list. `None` refuses connector
    /// calls.
    pub connectors: Option<&'a mcp::Catalog>,
    /// Where this project's commands run (PLAN 7.12). Only `shell_exec` reads
    /// it.
    pub exec_host: Option<&'a ExecHost>,
    /// The identity the call is made under. `None` applies no allow-list; only
    /// tests use it, since the turn loop always names one.
    pub identity: Option<Identity<'a>>,
    /// The decision model a `jev_ask` would name, for the dialog (PLAN 7.18).
    pub decision_model: Option<&'a str>,
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
            connectors: None,
            exec_host: None,
            delegated: false,
            unattended: false,
            identity: None,
            decision_model: None,
        }
    }

    /// Names the decision model a `jev_ask` dialog shows.
    #[must_use]
    pub const fn with_decision_model(mut self, model: Option<&'a str>) -> Self {
        self.decision_model = model;
        self
    }

    /// Marks this as a call made inside a delegated run.
    #[must_use]
    pub const fn delegated(mut self) -> Self {
        self.delegated = true;
        self
    }

    /// Marks this as a call made with nobody there to answer a dialog.
    #[must_use]
    pub const fn unattended(mut self, unattended: bool) -> Self {
        self.unattended = unattended;
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

    /// Supplies the connector catalog a connector call is judged against.
    #[must_use]
    pub const fn with_connectors(mut self, connectors: Option<&'a mcp::Catalog>) -> Self {
        self.connectors = connectors;
        self
    }

    /// Names where this project's commands run (PLAN 7.12).
    #[must_use]
    pub const fn with_exec_host(mut self, host: Option<&'a ExecHost>) -> Self {
        self.exec_host = host;
        self
    }

    /// Names the identity the call is made under, and the tools it holds.
    #[must_use]
    pub const fn with_identity(mut self, identity: Identity<'a>) -> Self {
        self.identity = Some(identity);
        self
    }
}

/// Decides what happens to one tool call: parse it, then [`decide_call`].
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
    // Owned: a connector call carries its own name, and the table takes `call`.
    let tool_name = call.tool().to_owned();

    // The allow-lists come before the workspace, which they do not depend on.
    // The model was never shown an ungranted tool, so this catches names from
    // an older transcript or invented ones.
    if let Some(identity) = ctx.identity {
        if !identity.allows(&tool_name) {
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

        // The skill allow-list (Phase 13), for the same reasons.
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

    // Delegation is one level deep (Phase 15, `COS.md` *Roles*), refused here
    // so no dialog offers a call that would be refused anyway.
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
                // A routine's run was allowed by what was signed on it, not by
                // a click, and the audit line says so.
                reason: if ctx.unattended {
                    "allowed by the routine's standing approval"
                } else {
                    "allowed for this session by an earlier approval"
                },
            }
        }
        // Nobody can answer (Phase 16). Checked last, so only a call that would
        // have opened a dialog is refused, and the refusal names the fix.
        _ if ctx.unattended => {
            tracing::info!(tool = tool_name, "an unattended run asked to ask");
            Decision::deny(
                ErrorCode::Denied,
                match &request.grant {
                    // The routine could have been signed for this.
                    Some(_) => format!(
                        "nobody is watching this run, and this routine was not signed for \
                         `{tool_name}` ({}). Do what you can without it, then return `blocked` \
                         and say what you needed",
                        request.summary
                    ),
                    // Nothing can be signed for this in advance (PLAN 3.1), so
                    // the model should stop looking for a way through.
                    None => format!(
                        "nobody is watching this run, and `{tool_name}` here is put to a person \
                         every time it is asked ({}), which no routine can be signed for in \
                         advance. Return `blocked` and say what you needed",
                        request.reason
                    ),
                },
            )
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
                // `E_DENIED`, not a code of its own: the model already knows
                // what a denial means.
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

    /// A workspace with a constitution in it and one declared source, already
    /// perceived. The world rows of PLAN 7.2 are the four assertions below.
    fn with_a_world() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let root = dunce::canonicalize(dir.path()).expect("canonical");

        std::fs::create_dir_all(root.join("world")).expect("world/");
        std::fs::create_dir_all(root.join("sources")).expect("sources/");
        // A cabinet beside it, so the tests can show what a world grant does
        // *not* cover.
        std::fs::create_dir_all(root.join(".aegis/artefacts")).expect("artefacts/");
        std::fs::create_dir_all(root.join(".aegis/status")).expect("status/");
        std::fs::write(root.join("world/essence.md"), "# Essence\n").expect("essence");

        let dump = root.join("sources/dump.sql");
        std::fs::write(&dump, "select 1;\n").expect("dump");
        let digest = format!(
            "{:x}",
            <sha2::Sha256 as sha2::Digest>::digest(b"select 1;\n")
        );
        std::fs::write(
            root.join("world/sources.yml"),
            format!("sources:\n  - path: sources/dump.sql\n    sha256: {digest}\n"),
        )
        .expect("sources.yml");

        (dir, root)
    }

    /// `COS.md` *Work*: specialists read the world and do not write it. Not an
    /// ask with a grant on offer — a refusal, in the same class as a reviewer
    /// calling `fs_write`.
    #[test]
    fn a_brief_cannot_amend_the_world() {
        let (_dir, root) = with_a_world();
        let grants = GrantStore::new();
        let ctx = PolicyCtx::new("s1", Some(&root), &grants).delegated();

        let call = json!({ "path": "world/essence.md", "content": "# Something else\n" });
        match decide(&ctx, tool::FS_WRITE, call) {
            Decision::Deny { code, reason } => {
                assert_eq!(code, ErrorCode::Denied);
                assert!(reason.contains("needs_you"), "{reason}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// Outside a brief, amending the world is asked at high risk, with a grant
    /// of its own that covers the constitution and nothing else.
    #[test]
    fn amending_the_world_is_asked_and_signed_for_on_its_own() {
        let (_dir, root) = with_a_world();
        let grants = GrantStore::new();
        let ctx = PolicyCtx::new("s1", Some(&root), &grants);

        let call = json!({ "path": "world/essence.md", "content": "# Something else\n" });
        match decide(&ctx, tool::FS_WRITE, call.clone()) {
            Decision::Ask { request, .. } => {
                assert_eq!(request.risk, Risk::High);
                assert_eq!(request.grant, Some(Grant::WorldAmend));
                assert!(request.reason.contains("constitution"), "{request:?}");
            }
            other => panic!("expected an ask, got {other:?}"),
        }

        // Signed once, the rest of the session's constitution writes go through.
        grants.insert("s1", Grant::WorldAmend);
        assert!(matches!(
            decide(&ctx, tool::FS_WRITE, call),
            Decision::Auto { .. }
        ));

        // And it covers only that. An ordinary write in the same workspace is
        // still asked about, because the two rows offer different grants.
        let elsewhere = json!({ "path": ".aegis/artefacts/note.md", "content": "hello\n" });
        match decide(&ctx, tool::FS_WRITE, elsewhere) {
            Decision::Ask { request, .. } => assert_eq!(request.grant, Some(Grant::FsWrite)),
            other => panic!("expected the ordinary write row, got {other:?}"),
        }
    }

    /// The other direction, and the one that matters more: allowing writes so a
    /// session can file its artefacts is not agreeing to let it rewrite what
    /// the project *is*.
    #[test]
    fn an_ordinary_write_grant_does_not_reach_the_world() {
        let (_dir, root) = with_a_world();
        let grants = GrantStore::new();
        grants.insert("s1", Grant::FsWrite);
        let ctx = PolicyCtx::new("s1", Some(&root), &grants);

        assert!(
            matches!(
                decide(
                    &ctx,
                    tool::FS_WRITE,
                    json!({ "path": ".aegis/status/STATUS.md", "content": "# Status\n" })
                ),
                Decision::Auto { .. }
            ),
            "the grant covers what it was created for"
        );

        match decide(
            &ctx,
            tool::FS_WRITE,
            json!({ "path": "world/essence.md", "content": "# Something else\n" }),
        ) {
            Decision::Ask { request, .. } => {
                assert_eq!(request.grant, Some(Grant::WorldAmend), "{request:?}");
            }
            other => panic!("the world is still asked about, got {other:?}"),
        }

        // Only the first segment of a path is the constitution, so a repository
        // with its own `src/world/` is untouched by any of this.
        std::fs::create_dir_all(root.join("src/world")).expect("a module of that name");
        assert!(matches!(
            decide(
                &ctx,
                tool::FS_WRITE,
                json!({ "path": "src/world/mod.rs", "content": "//! not one\n" })
            ),
            Decision::Auto { .. }
        ));
    }

    /// `COS.md`: amending the world is a *human* decision. A scheduled run is
    /// the one run with no human in it, so it is offered nothing to sign and
    /// refused — the second of two places, the first being the door in
    /// `schedule::check` when the routine is saved.
    #[test]
    fn a_routine_is_offered_nothing_to_sign_for_the_world() {
        let (_dir, root) = with_a_world();
        let grants = GrantStore::new();
        let ctx = PolicyCtx::new("s1", Some(&root), &grants).unattended(true);

        match decide(
            &ctx,
            tool::FS_WRITE,
            json!({ "path": "world/essence.md", "content": "# Something else\n" }),
        ) {
            Decision::Deny { code, reason } => {
                assert_eq!(code, ErrorCode::Denied);
                assert!(reason.contains("nobody is watching"), "{reason}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// The row PLAN 7.2 adds to the read side: a declared source that has not
    /// moved is denied rather than asked about, because what it says is already
    /// in `world/`.
    #[test]
    fn a_source_already_perceived_is_refused_and_a_moved_one_reads() {
        let (_dir, root) = with_a_world();
        let grants = GrantStore::new();
        let ctx = PolicyCtx::new("s1", Some(&root), &grants);

        match decide(&ctx, tool::FS_READ, json!({ "path": "sources/dump.sql" })) {
            Decision::Deny { code, reason } => {
                assert_eq!(code, ErrorCode::Denied);
                assert!(reason.contains("sources/dump.sql"), "{reason}");
                assert!(reason.contains("world/"), "{reason}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }

        // The delta is the one legitimate re-perception, so once the operator
        // drops a new dump the same read is an ordinary contained one again.
        std::fs::write(root.join("sources/dump.sql"), "select 1;\nselect 2;\n").expect("write");
        assert!(matches!(
            decide(&ctx, tool::FS_READ, json!({ "path": "sources/dump.sql" })),
            Decision::Auto { .. }
        ));
    }

    /// And none of it applies to a workspace that never opted in. A folder with
    /// no `world/` is judged exactly as it was before this slice.
    #[test]
    fn a_workspace_with_no_world_is_judged_as_it_always_was() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let root = dunce::canonicalize(dir.path()).expect("canonical");
        std::fs::create_dir_all(root.join("world")).expect("an empty folder of that name");
        std::fs::write(root.join("sources.txt"), "a dump nobody declared").expect("write");

        let grants = GrantStore::new();
        let ctx = PolicyCtx::new("s1", Some(&root), &grants);

        assert!(matches!(
            decide(&ctx, tool::FS_READ, json!({ "path": "sources.txt" })),
            Decision::Auto { .. }
        ));
        // An empty `world/` is not a world, but the gate reads the path, so the
        // first file of a world is asked about like every later one.
        match decide(
            &ctx,
            tool::FS_WRITE,
            json!({ "path": "world/essence.md", "content": "# Essence\n" }),
        ) {
            Decision::Ask { request, .. } => {
                assert_eq!(request.grant, Some(Grant::WorldAmend));
                assert_eq!(request.risk, Risk::High);
            }
            other => panic!("expected an ask, got {other:?}"),
        }
    }
    /// Without a host nothing moves: the row a `shell_exec` produces is the one
    /// every phase before PLAN 7.12 produced, and the dialog has one working
    /// directory to show because there is one.
    #[test]
    fn a_project_on_this_computer_draws_the_row_it_always_did() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let root = dunce::canonicalize(dir.path()).expect("canonical");
        let grants = GrantStore::new();
        let ctx = PolicyCtx::new("s1", Some(&root), &grants);

        match decide(&ctx, tool::SHELL_EXEC, json!({ "program": "git" })) {
            Decision::Ask { call, request } => {
                assert!(matches!(call, ResolvedCall::ShellExec { host: None, .. }));
                match request.detail {
                    ApprovalDetail::Shell { host, cwd, .. } => {
                        assert_eq!(host, None);
                        assert_eq!(cwd, root.display().to_string());
                    }
                    other => panic!("expected a shell row, got {other:?}"),
                }
            }
            other => panic!("expected an ask, got {other:?}"),
        }
    }

    /// With a host, the dialog names the distribution and the Linux directory
    /// beside the Windows folder, and the grant is still keyed on the program.
    #[cfg(windows)]
    #[test]
    fn a_wsl_host_puts_the_distro_and_the_linux_directory_in_the_dialog() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let root = dunce::canonicalize(dir.path()).expect("canonical");
        let grants = GrantStore::new();
        let host = crate::exec_host::ExecHost::Wsl {
            distro: "Ubuntu".to_owned(),
        };
        let ctx = PolicyCtx::new("s1", Some(&root), &grants).with_exec_host(Some(&host));

        let expected = crate::exec_host::linux_path("Ubuntu", &root)
            .expect("a temp dir is on a drive, and a drive is automounted");

        match decide(&ctx, tool::SHELL_EXEC, json!({ "program": "git" })) {
            Decision::Ask { call, request } => {
                match &call {
                    ResolvedCall::ShellExec { host, program, .. } => {
                        assert_eq!(program, "git", "the model still names the program");
                        let host = host.as_ref().expect("a host reaches the tool");
                        assert_eq!(host.distro, "Ubuntu");
                        assert_eq!(host.cwd, expected);
                    }
                    other => panic!("expected a shell call, got {other:?}"),
                }
                assert_eq!(request.grant, Some(Grant::shell("git")));
                assert!(request.reason.contains("Ubuntu"), "{}", request.reason);
                match request.detail {
                    ApprovalDetail::Shell { host, cwd, .. } => {
                        assert_eq!(
                            host.map(|host| host.cwd),
                            Some(expected),
                            "the dialog shows the directory the command starts in"
                        );
                        assert_eq!(
                            cwd,
                            root.display().to_string(),
                            "and the folder the file tools use, beside it"
                        );
                    }
                    other => panic!("expected a shell row, got {other:?}"),
                }
            }
            other => panic!("expected an ask, got {other:?}"),
        }
    }

    /// A folder of another distribution is refused before anyone is asked: no
    /// answer could make the command runnable, and running it here instead is
    /// not offered.
    #[cfg(windows)]
    #[tokio::test]
    async fn a_folder_of_another_distro_is_refused_rather_than_asked_about() {
        let Some(installed) = crate::exec_host::installed().await.into_iter().next() else {
            return;
        };

        let dir = tempfile::TempDir::new().expect("temp dir");
        let root = dunce::canonicalize(dir.path()).expect("canonical");
        let grants = GrantStore::new();
        let host = crate::exec_host::ExecHost::Wsl {
            distro: "aegis-not-a-distro".to_owned(),
        };
        let ctx = PolicyCtx::new("s1", Some(&root), &grants).with_exec_host(Some(&host));

        match decide(
            &ctx,
            tool::SHELL_EXEC,
            json!({ "program": "git", "cwd": format!(r"\\wsl$\{installed}\etc") }),
        ) {
            Decision::Deny { code, reason } => {
                assert_eq!(code, ErrorCode::ExecHost, "{reason}");
                assert!(reason.contains("aegis-not-a-distro"), "{reason}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// The same rule arriving by a different road. A build with no WSL cannot
    /// honour a WSL host, and every folder on it is one the distribution has no
    /// path for — so the refusal is the whole behaviour, and it is a refusal
    /// rather than the command quietly running here.
    #[cfg(not(windows))]
    #[test]
    fn a_wsl_host_on_a_build_without_wsl_refuses_before_asking() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let root = dunce::canonicalize(dir.path()).expect("canonical");
        let grants = GrantStore::new();
        let host = crate::exec_host::ExecHost::Wsl {
            distro: "Ubuntu".to_owned(),
        };
        let ctx = PolicyCtx::new("s1", Some(&root), &grants).with_exec_host(Some(&host));

        match decide(&ctx, tool::SHELL_EXEC, json!({ "program": "git" })) {
            Decision::Deny { code, reason } => {
                assert_eq!(code, ErrorCode::ExecHost);
                assert!(reason.contains("Ubuntu"), "{reason}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
}
