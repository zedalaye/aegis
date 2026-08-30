//! The tool registry: declaration, schema export, dispatch and the envelope.
//!
//! A tool is declared exactly once, as a [`ToolSpec`]. Its name is the same
//! constant the policy table keys on and the audit log records, so a tool's
//! schema, its dispatch and its permission row cannot drift apart — the thing
//! that would otherwise let a tool exist that nothing gates (PLAN 4.1).
//!
//! The shape of the pipeline is:
//!
//! ```text
//! model args ──▶ policy::decide ──▶ ResolvedCall ──▶ tools::run ──▶ ToolResult
//!                      │                                  │
//!                      └── AskRequest (Phase 6)           └── one audit line
//! ```
//!
//! Two things follow from where the arrow starts. A tool never sees the
//! strings the model sent: it takes a [`ResolvedCall`], whose paths policy
//! already resolved and judged, so "policy checked one path and the tool
//! opened another" is not a state this code can reach. And every exit from
//! [`run`] writes exactly one audit line, because the audit call is here, in
//! the one function every tool goes through, rather than in each tool.
//!
//! The envelope is PLAN 4.3: one shape for success and failure alike, so the
//! model never has to guess which it got. A denial is an ordinary envelope
//! with `ok: false` and `E_DENIED`, not an exception — the model reads it,
//! explains itself and tries something else, and the turn keeps going.
//!
//! Every tool the MVP names is here: `fs_list`, `fs_read`, `fs_write`,
//! `shell_exec` and, since Phase 9, `screen_capture`; since Phase 13,
//! `skill_run` and `skill_return` beside them. The registry and the decision
//! table are the same list, which is what makes "a tool nothing gates" a thing
//! this code cannot express — and it is why the two skill tools are entries
//! here rather than a channel of their own. A skill is not a tool, but *asking
//! for a runbook* is a verb like any other, and putting it anywhere else would
//! mean a second dispatch path with a second place to remember the audit line.
//!
//! [`run`] is `async` because of the two tools that reach outside this process.
//! The filesystem tools are short, local and synchronous, and are called
//! inline. A child process is none of those, and it has to be awaited inside
//! the same cancellation as the turn or a two-minute command would be a
//! two-minute stall with a Stop button that does nothing. A capture is quick
//! but it is a round trip to the window server — and on a compositor that
//! shows its own consent prompt, a round trip through a person — so it goes to
//! a blocking thread rather than parking a runtime worker on it.

pub mod fs;
pub mod screenshot;
pub mod shell;
pub mod skill;

use std::fmt;
use std::path::Path;
use std::time::Instant;

use serde::{Deserialize, Serialize, Serializer};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

use crate::audit::{AuditArtifact, AuditDecision, AuditEntry, AuditLog, AuditRecord, Outcome};
use crate::error::ErrorCode;
use crate::policy::{tool, ResolvedCall};
use crate::skills::SkillCtx;

/// Most bytes `fs_read` will return in one envelope (PLAN 4.3).
pub const READ_MAX_BYTES: u64 = 256 * 1024;

/// Most entries `fs_list` will return in one envelope (PLAN 4.3).
pub const LIST_MAX_ENTRIES: u32 = 1000;

/// Most bytes of combined stdout and stderr `shell_exec` will return in one
/// envelope (PLAN 4.3).
pub const EXEC_MAX_BYTES: u64 = 64 * 1024;

// ---------------------------------------------------------------------------
// Progress
// ---------------------------------------------------------------------------

/// Which pipe a chunk of tool output came from (PLAN 2.2, `tool:progress`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "bindings.ts")]
pub enum Stream {
    /// The child's standard output.
    Stdout,
    /// The child's standard error.
    Stderr,
}

/// Where a running tool's output goes while it is still running.
///
/// Only `shell_exec` produces any: a file is read in one call, but a command
/// can take two minutes, and a progress pane that only fills in at the end is
/// indistinguishable from a hang.
///
/// The tool hands over text; the runtime numbers it. That split is deliberate
/// — `seq` is a property of the event stream, not of the child process, and a
/// tool that assigned its own would have to know what else the turn had
/// already emitted.
pub trait ProgressSink: Send + Sync {
    /// Delivers one frame of output. Never fails, for the same reason
    /// [`EventSink`](crate::agent::EventSink) never does: a UI that missed a
    /// frame is cosmetic, and a command killed because a window closed is not.
    fn chunk(&self, stream: Stream, text: &str);
}

/// A [`ProgressSink`] that drops everything.
///
/// For a tool call with nobody watching — a test, or a future background run
/// with no window open.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullProgress;

impl ProgressSink for NullProgress {
    fn chunk(&self, _stream: Stream, _text: &str) {}
}

// ---------------------------------------------------------------------------
// The envelope
// ---------------------------------------------------------------------------

/// What the model sees for a tool call — every tool, every outcome (PLAN 4.3).
///
/// One shape rather than a success type and an error type, because the model
/// reads this as text in a `tool` message and branching on a shape it has to
/// recognize first is exactly the ambiguity that makes a model hallucinate a
/// result. `ok` says which it is; `error` is present precisely when `ok` is
/// false.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolResult {
    /// Whether the tool did what was asked.
    pub ok: bool,
    /// Which tool answered.
    pub tool: String,
    /// The result as text, already truncated to the tool's cap.
    pub content: String,
    /// Whether [`ToolResult::content`] is shorter than what was available.
    pub truncated: bool,
    /// The size of the thing before truncation.
    ///
    /// What it counts is the tool's own: the file's size for `fs_read`, the
    /// bytes written for `fs_write`, the rendered length of the listing for
    /// `fs_list`. Each tool documents it.
    pub bytes: u64,
    /// Per-tool detail — an exit code, a duration, the path that was touched.
    pub meta: Value,
    /// Why this failed, when it did.
    pub error: Option<ToolError>,
}

/// The failure half of an envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolError {
    /// The stable code (PLAN 4.4). The model can key on this.
    #[serde(serialize_with = "code_as_str")]
    pub code: ErrorCode,
    /// What went wrong, in one sentence, written to be read by the model and
    /// shown in the transcript.
    pub message: String,
}

/// Serializes an [`ErrorCode`] as its wire string.
fn code_as_str<S: Serializer>(code: &ErrorCode, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(code.as_str())
}

impl ToolResult {
    /// A success.
    fn success(tool: &str, content: String, bytes: u64, truncated: bool, meta: Value) -> Self {
        Self {
            ok: true,
            tool: tool.to_owned(),
            content,
            truncated,
            bytes,
            meta,
            error: None,
        }
    }

    /// A failure. `content` stays empty: there is nothing to report but the
    /// error, and a model given both tends to read the content and ignore the
    /// code.
    ///
    /// [`ToolResult::refusal`] is the same thing, reachable from outside this
    /// module.
    fn failure(tool: &str, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            tool: tool.to_owned(),
            content: String::new(),
            truncated: false,
            bytes: 0,
            meta: json!({}),
            error: Some(ToolError {
                code,
                message: message.into(),
            }),
        }
    }

    /// An envelope for a call that never ran.
    ///
    /// The turn loop needs this for the refusals it makes on its own account —
    /// arguments that never parsed, a call abandoned by a cancel — which are
    /// answered without ever reaching [`run`] and so have no audit line of
    /// their own to carry the message.
    pub fn refusal(tool: &str, code: ErrorCode, message: impl Into<String>) -> Self {
        Self::failure(tool, code, message)
    }

    /// An envelope built directly, for tests of the runtime around tools.
    ///
    /// Only the three fields the turn loop branches on. Everything else that
    /// wants an envelope gets one by running a tool, which is the point.
    #[cfg(test)]
    pub(crate) fn for_test(ok: bool, tool: &str, meta: Value) -> Self {
        Self {
            ok,
            tool: tool.to_owned(),
            content: String::new(),
            truncated: false,
            bytes: 0,
            meta,
            error: None,
        }
    }

    /// The envelope as the JSON string that goes into a `tool` message.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            // Unreachable for this shape — every field is a plain string,
            // number, bool or `serde_json::Value`. A model still has to
            // receive *something* parseable if it ever happens.
            r#"{"ok":false,"error":{"code":"E_TOOL_FAILED","message":"the result could not be rendered"}}"#
                .to_owned()
        })
    }
}

/// What one tool call produced: the envelope, plus what the runtime around it
/// needs.
///
/// The extra fields are not part of what the model sees. `summary` is the
/// one-line result the transcript and the `tool:finished` event carry, and the
/// byte counts are the audit log's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutcome {
    /// The envelope for the model.
    pub result: ToolResult,
    /// One human line: `read src/main.rs (2.4 KB)`. Never a raw blob.
    pub summary: String,
    /// A local image this call produced, for the transcript to show.
    ///
    /// Only `screen_capture` fills it. It is a path and not bytes on purpose
    /// (PLAN 5.4): the WebView loads the file through the asset protocol,
    /// which is scoped to the capture directory, rather than having a
    /// megabyte of base64 pushed through the event channel.
    pub image_path: Option<String>,
    /// The audit line that was written for this call.
    pub audit: AuditEntry,
}

/// What a tool produced before the runtime wrapped it.
///
/// Internal to this module and to [`fs`]: it is [`ToolOutcome`] minus
/// everything a tool has no business knowing about — the session, the audit
/// log, how long the call took.
pub(crate) struct Produced {
    /// The envelope.
    result: ToolResult,
    /// The one-line summary.
    summary: String,
    /// Bytes the call carried in.
    bytes_in: u64,
    /// Bytes the call produced.
    bytes_out: u64,
    /// What the audit line should say, when `ok` alone does not say it.
    ///
    /// `None` means the ordinary reading: a successful envelope is
    /// [`Outcome::Ok`] and a failed one is [`Outcome::Error`]. The one tool
    /// that needs more is `shell_exec`, whose command can be killed by a
    /// cancel — that is neither a tool that failed nor one that was refused,
    /// and the log has a word for it.
    outcome: Option<Outcome>,
    /// The file the call left on disk, when it left one.
    ///
    /// Set by `screen_capture` and by nothing else so far. It reaches the
    /// audit line and the transcript from here, which is why the tool does not
    /// have to know about either.
    artifact: Option<AuditArtifact>,
    /// The skill this call belongs to, when the call itself says which.
    ///
    /// Only `skill_run` sets it, and only because it is the call that *opens*
    /// a run: nothing was running when it was made, so [`ToolCtx::skills`]
    /// would put no name on its line and the opening of a run would be the one
    /// event of a run that is not on the record. Every later call in the span
    /// gets the name from the context instead.
    skill: Option<String>,
}

impl Produced {
    /// A successful tool run.
    pub(crate) fn ok(
        tool: &str,
        summary: impl Into<String>,
        content: String,
        bytes: u64,
        truncated: bool,
        meta: Value,
    ) -> Self {
        let bytes_out = content.len() as u64;
        Self {
            result: ToolResult::success(tool, content, bytes, truncated, meta),
            summary: summary.into(),
            bytes_in: 0,
            bytes_out,
            outcome: None,
            artifact: None,
            skill: None,
        }
    }

    /// A tool that ran and failed on its own terms.
    pub(crate) fn failed(tool: &str, code: ErrorCode, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            result: ToolResult::failure(tool, code, message.clone()),
            summary: message,
            bytes_in: 0,
            bytes_out: 0,
            outcome: None,
            artifact: None,
            skill: None,
        }
    }

    /// A tool that was still running when the turn was cancelled.
    pub(crate) fn cancelled(tool: &str, message: impl Into<String>) -> Self {
        Self {
            outcome: Some(Outcome::Cancelled),
            ..Self::failed(tool, ErrorCode::Cancelled, message)
        }
    }

    /// Records what the call carried in — a write's content, and nothing else
    /// in this phase.
    pub(crate) const fn with_bytes_in(mut self, bytes: u64) -> Self {
        self.bytes_in = bytes;
        self
    }

    /// Records the file the call wrote.
    pub(crate) fn with_artifact(mut self, artifact: AuditArtifact) -> Self {
        self.artifact = Some(artifact);
        self
    }

    /// Names the skill run this call opened.
    pub(crate) fn in_skill(mut self, name: &str) -> Self {
        self.skill = Some(name.to_owned());
        self
    }
}

// ---------------------------------------------------------------------------
// The registry
// ---------------------------------------------------------------------------

/// One tool, declared once.
///
/// `parameters` is the JSON Schema the model is given. It is deliberately
/// stricter than what [`ToolCall::parse`](crate::policy::ToolCall::parse)
/// accepts: the schema forbids unknown keys so a model has a clear contract to
/// follow, while the parser ignores them so a model that adds a stray one is
/// answered rather than stalled. Strict in the description, forgiving at the
/// door.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    /// The name, shared with the policy table and the audit log.
    pub name: &'static str,
    /// What the tool does, written for the model.
    pub description: &'static str,
    /// JSON Schema for the arguments.
    pub parameters: fn() -> Value,
}

impl ToolSpec {
    /// The tool as one entry of an OpenAI-compatible `tools` array
    /// (PLAN 4.1).
    pub fn to_schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": (self.parameters)(),
            }
        })
    }
}

/// Every tool this build can actually run.
///
/// The order is the order the model sees them in, which is the order they are
/// most likely to be needed: look, read, then change something.
pub fn registry() -> &'static [ToolSpec] {
    &[
        ToolSpec {
            name: tool::FS_LIST,
            description: "List the entries of a directory in the workspace. Directories are \
                          shown with a trailing slash, files with their size in bytes, symbolic \
                          links with a trailing @. Paths may be relative to the workspace root.",
            parameters: fs::list_schema,
        },
        ToolSpec {
            name: tool::FS_READ,
            description: "Read a UTF-8 text file. Returns at most 256 KB from `offset`; the \
                          envelope says whether more remains. Paths may be relative to the \
                          workspace root.",
            parameters: fs::read_schema,
        },
        ToolSpec {
            name: tool::FS_WRITE,
            description: "Write a UTF-8 text file, replacing it if it exists. Writes the content \
                          exactly, adding nothing. Paths may be relative to the workspace root.",
            parameters: fs::write_schema,
        },
        ToolSpec {
            name: tool::SHELL_EXEC,
            description: "Run a program in the workspace and return its output. There is no \
                          shell: `program` is looked up on PATH and spawned with `args` as a \
                          vector, so pipes, redirection, globs, `&&` and variable expansion do \
                          not work — run one program per call and compose the steps yourself. \
                          stdout and stderr come back interleaved, capped at 64 KB, and \
                          `meta.exit_code` says how the program ended.",
            parameters: shell::exec_schema,
        },
        ToolSpec {
            name: tool::SCREEN_CAPTURE,
            description: "Capture the primary display and write it to a PNG outside the                           workspace. Returns the file's path, its pixel size and a SHA-256 —                           never the image, which this build cannot read back, so do not expect                           to see what was on the screen. Every capture is approved by the user                           first and holds whatever was on that display, so ask for one only                           when the user has asked to be looked at.",
            parameters: screenshot::capture_schema,
        },
        ToolSpec {
            name: tool::SKILL_RUN,
            description: "Load a skill's runbook into this turn. A skill is a procedure someone \
                          already wrote down; the catalog in your instructions says which ones \
                          you may run and what each is for. This returns the steps — it does not \
                          carry any of them out, and it grants you nothing: every step is an \
                          ordinary tool call, put to the user exactly as it would be otherwise. \
                          The steps last for this turn only. Finish a run with `skill_return`.",
            parameters: skill::run_schema,
        },
        ToolSpec {
            name: tool::SKILL_RETURN,
            description: "Close the skill you loaded and record what came of it. `status` is \
                          `done`, `blocked` or `needs_you`; `summary` is five lines at most; \
                          `artefacts` are paths inside the workspace, and they are checked — a \
                          `done` naming a file that is not on disk is refused. A `blocked` or a \
                          `needs_you` needs at least one `open_questions` entry.",
            parameters: skill::return_schema,
        },
    ]
}

/// The `tools` array for a model request (PLAN 4.1).
pub fn schemas() -> Vec<Value> {
    registry().iter().map(ToolSpec::to_schema).collect()
}

/// The `tools` array for a model request, narrowed to one identity's
/// allow-list (PLAN 7.3, Phase 12).
///
/// Filtering the *schemas* is half of what a tool ACL is. A model cannot ask
/// for a function it was never shown, so an identity that was not granted
/// `shell_exec` does not spend a turn discovering that it may not run one — it
/// simply has no such tool. The other half is policy refusing the call anyway
/// ([`decide_call`](crate::policy::decide_call)), because a transcript carries
/// the tools of the turn that wrote it and an identity's grants can be edited
/// between two turns.
///
/// Registry order is preserved rather than the allow-list's, so the order the
/// model reads them in is the registry's regardless of how the list was typed.
pub fn schemas_for(allowed: &[String]) -> Vec<Value> {
    registry()
        .iter()
        .filter(|spec| allowed.iter().any(|name| name == spec.name))
        .map(ToolSpec::to_schema)
        .collect()
}

/// Every tool name this build can run, in registry order.
///
/// The vocabulary an identity's allow-list is validated against
/// ([`store::agents`](crate::store::agents)), so a grant of a tool that does
/// not exist is not a thing that can be stored.
pub fn names() -> Vec<&'static str> {
    registry().iter().map(|spec| spec.name).collect()
}

/// Looks a tool up by name.
pub fn spec(name: &str) -> Option<&'static ToolSpec> {
    registry().iter().find(|spec| spec.name == name)
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Everything a tool call needs from the runtime around it.
///
/// The ids are the model's and the turn loop's; `args` is what the model
/// actually sent, kept for the audit line so the log records the call as it
/// was made rather than as policy rewrote it.
#[derive(Clone, Copy)]
pub struct ToolCtx<'a> {
    /// Which session made the call. Every audit line carries it.
    pub session_id: &'a str,
    /// Which identity it was made as (PLAN 7.3, Phase 12).
    ///
    /// Carried for the log rather than for the call: whether the identity may
    /// use this tool was settled by policy before anything got here, and a tool
    /// that re-checked would be a second copy of a rule that can then disagree
    /// with the first.
    pub agent_id: &'a str,
    /// Which turn within the session.
    pub turn_id: &'a str,
    /// The model's own id for this call.
    pub call_id: &'a str,
    /// Where the audit line goes.
    pub audit: &'a AuditLog,
    /// The directory captures are written to.
    ///
    /// Passed in rather than derived, and deliberately not part of the
    /// resolved call: where Aegis keeps its own artefacts is a fact about the
    /// installation, not about what the model asked for, and policy has no
    /// opinion about a path the model never named.
    pub captures: &'a Path,
    /// The arguments as the model sent them, for the digest and the redacted
    /// copy.
    pub args: &'a Value,
    /// Where a running tool's output goes while it is still running.
    pub progress: &'a dyn ProgressSink,
    /// The turn's cancellation token.
    ///
    /// A tool that can outlive a click on Stop has to hold this, or Stop
    /// becomes a button with no effect the user can see. Only `shell_exec`
    /// reads it today; everything else finishes faster than a person can ask
    /// it not to.
    pub cancel: &'a CancellationToken,
    /// Where the skill library is, and which run is open (PLAN 7.3, Phase 13).
    ///
    /// Held here for the reason `captures` is: where Aegis keeps runbooks and
    /// which identity is running are facts about the installation and the
    /// session, not about what the model asked for. `active` is also what puts
    /// the skill's name on *every* audit line of a run, not only the two the
    /// skill tools make — which is what makes a run budgetable and replayable
    /// afterwards (PLAN 7.6, *Audit names the skill*).
    pub skills: SkillCtx<'a>,
}

// Written out rather than derived: `&dyn ProgressSink` has no `Debug`, and
// demanding one of every sink would be a constraint on implementors for the
// sake of one line of diagnostics.
impl fmt::Debug for ToolCtx<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolCtx")
            .field("session_id", &self.session_id)
            .field("agent_id", &self.agent_id)
            .field("turn_id", &self.turn_id)
            .field("call_id", &self.call_id)
            .field("captures", &self.captures)
            .field("skill", &self.skills.active)
            .field("cancelled", &self.cancel.is_cancelled())
            .finish_non_exhaustive()
    }
}

/// Runs a call that policy — or the user — has cleared, and audits it.
///
/// This is the only way a tool runs. `decision` and `reason` are what the
/// audit line records about *why* it ran: `Auto` with policy's own reason for
/// an auto-allowed call, `AllowOnce` or `AllowSession` with the reason the
/// user was shown when they approved it.
///
/// Never returns an `Err`: a tool that fails produces an envelope saying so
/// (PLAN 4.3), because the turn continues either way and the model is the one
/// that has to react.
pub async fn run(
    ctx: &ToolCtx<'_>,
    decision: AuditDecision,
    reason: &str,
    call: &ResolvedCall,
) -> ToolOutcome {
    let name = call.tool();
    let started = Instant::now();

    let produced = match call {
        ResolvedCall::FsList { path, max_entries } => fs::list(path, *max_entries),
        ResolvedCall::FsRead {
            path,
            offset,
            limit,
        } => fs::read(path, *offset, *limit),
        ResolvedCall::FsWrite {
            path,
            content,
            create_dirs,
        } => fs::write(path, content, *create_dirs),
        ResolvedCall::ShellExec {
            program,
            args,
            cwd,
            timeout_ms,
        } => shell::exec(program, args, cwd, *timeout_ms, ctx.progress, ctx.cancel).await,
        // On a blocking thread, not inline: a capture is a round trip to the
        // window server, and on a compositor that raises its own consent
        // prompt it is a round trip through a person. Neither belongs on a
        // runtime worker. Nothing is passed by reference, because the work
        // outlives this stack frame.
        ResolvedCall::ScreenCapture { display } => {
            let display = display.clone();
            let dir = ctx.captures.to_path_buf();

            match tokio::task::spawn_blocking(move || screenshot::capture(&display, &dir)).await {
                Ok(produced) => produced,
                // The capture thread panicked. Nothing here can say what the
                // window server did, and the model still needs an answer.
                Err(err) => {
                    tracing::error!(%err, "the capture thread did not return");
                    Produced::failed(
                        name,
                        ErrorCode::ToolFailed,
                        "the capture did not complete".to_owned(),
                    )
                }
            }
        }
        // Neither of these reaches outside the process: one reads a runbook,
        // the other checks a report against `COS.md`'s shape. They run inline
        // like the filesystem tools, and for the same reason.
        ResolvedCall::SkillRun { name } => skill::run(name, ctx.skills),
        ResolvedCall::SkillReturn { report } => skill::ret(report, ctx.skills),
    };

    // `as` saturates at `u64::MAX` here, which is 584 million years: the cast
    // cannot be the thing that goes wrong in this line.
    #[allow(clippy::cast_possible_truncation)]
    let duration_ms = started.elapsed().as_millis() as u64;

    let audit = ctx.audit.append(&AuditRecord {
        session_id: ctx.session_id,
        agent_id: ctx.agent_id,
        turn_id: ctx.turn_id,
        call_id: ctx.call_id,
        tool: name,
        // The call's own claim first, then the run it is inside. Only
        // `skill_run` makes one, and only for its own line: it opens the run,
        // so nothing was active when it was judged (see `Produced::skill`).
        skill: produced
            .skill
            .as_deref()
            .or(ctx.skills.active)
            .unwrap_or(""),
        decision,
        policy_reason: reason,
        args: ctx.args,
        outcome: produced.outcome.unwrap_or(if produced.result.ok {
            Outcome::Ok
        } else {
            Outcome::Error
        }),
        duration_ms,
        bytes_in: produced.bytes_in,
        bytes_out: produced.bytes_out,
        error_code: produced.result.error.as_ref().map(|error| error.code),
        artifact: produced.artifact.clone(),
    });

    tracing::info!(
        tool = name,
        ok = produced.result.ok,
        duration_ms,
        "tool call finished"
    );

    ToolOutcome {
        result: produced.result,
        summary: produced.summary,
        image_path: produced.artifact.map(|artifact| artifact.path),
        audit,
    }
}

/// Records a call that never ran, and produces the envelope the model sees.
///
/// Used for both kinds of refusal: policy's hard denials (PLAN 3.2), which
/// never reach a user, and a user's `deny` on an approval. They are one code
/// path because they are one thing to the model and one line in the log — the
/// difference between them is `decision`, which is exactly what the log is
/// there to record.
///
/// `code` is usually [`ErrorCode::Denied`]; a hard denial carries the more
/// specific code policy chose, such as `E_PATH_OUTSIDE_WORKSPACE`.
pub fn refuse(
    ctx: &ToolCtx<'_>,
    tool_name: &str,
    decision: AuditDecision,
    code: ErrorCode,
    reason: &str,
) -> ToolOutcome {
    let audit = ctx.audit.append(&AuditRecord {
        session_id: ctx.session_id,
        agent_id: ctx.agent_id,
        turn_id: ctx.turn_id,
        call_id: ctx.call_id,
        tool: tool_name,
        // A refusal inside a run belongs to that run: "what did this skill try
        // and get told no about" is exactly what a replay has to answer.
        skill: ctx.skills.active.unwrap_or(""),
        decision,
        policy_reason: reason,
        args: ctx.args,
        outcome: Outcome::Denied,
        duration_ms: 0,
        bytes_in: 0,
        bytes_out: 0,
        error_code: Some(code),
        // A call that never ran wrote nothing. This is the one place that is
        // worth stating rather than defaulting: a refusal that still carried
        // an artefact would mean a file on disk nobody approved.
        artifact: None,
    });

    tracing::info!(tool = tool_name, code = %code, "tool call refused");

    ToolOutcome {
        result: ToolResult::failure(tool_name, code, reason),
        summary: reason.to_owned(),
        image_path: None,
        audit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::policy;

    #[test]
    fn every_registered_tool_has_a_policy_row() {
        // The registry and the decision table key off the same constants; a
        // tool whose name is not one of them would be a tool nothing gates.
        let known = [
            tool::FS_LIST,
            tool::FS_READ,
            tool::FS_WRITE,
            tool::SHELL_EXEC,
            tool::SCREEN_CAPTURE,
            tool::SKILL_RUN,
            tool::SKILL_RETURN,
        ];

        for spec in registry() {
            assert!(
                known.contains(&spec.name),
                "`{}` is offered to the model but policy has no row for it",
                spec.name
            );
        }
    }

    #[test]
    fn every_registered_schema_parses_as_the_call_it_describes() {
        // A schema whose name policy cannot parse would be a tool the model is
        // told about and the runtime rejects.
        for spec in registry() {
            let schema = spec.to_schema();
            assert_eq!(schema["function"]["name"], spec.name);
            assert_eq!(schema["function"]["parameters"]["type"], "object");
            assert!(
                !spec.description.is_empty(),
                "`{}` has no description",
                spec.name
            );
        }
    }

    #[test]
    fn a_schema_declares_its_required_arguments() {
        let read = spec(tool::FS_READ).expect("fs_read is registered");
        let schema = (read.parameters)();

        assert_eq!(schema["required"], json!(["path"]));
        assert_eq!(schema["additionalProperties"], json!(false));
    }

    /// The registry and the decision table are one list. A tool in the table
    /// and not the registry would be a tool the model cannot reach; one in the
    /// registry and not the table would be a tool nothing gates.
    #[test]
    fn every_tool_the_matrix_judges_is_a_tool_the_model_is_offered() {
        let offered: Vec<&str> = registry().iter().map(|spec| spec.name).collect();

        for name in [
            tool::FS_LIST,
            tool::FS_READ,
            tool::FS_WRITE,
            tool::SHELL_EXEC,
            tool::SCREEN_CAPTURE,
        ] {
            assert!(offered.contains(&name), "`{name}` is gated but not offered");
        }
    }

    #[test]
    fn an_envelope_renders_the_documented_shape() {
        let result = ToolResult::success(
            tool::FS_READ,
            "hello".to_owned(),
            5,
            false,
            json!({ "path": "a.txt" }),
        );
        let json: Value = serde_json::from_str(&result.to_json()).expect("valid JSON");

        assert_eq!(json["ok"], true);
        assert_eq!(json["tool"], "fs_read");
        assert_eq!(json["content"], "hello");
        assert_eq!(json["truncated"], false);
        assert_eq!(json["bytes"], 5);
        assert_eq!(json["meta"]["path"], "a.txt");
        assert_eq!(json["error"], Value::Null);
    }

    #[test]
    fn a_denial_is_an_ordinary_envelope() {
        let result = ToolResult::failure(tool::FS_WRITE, ErrorCode::Denied, "user denied");
        let json: Value = serde_json::from_str(&result.to_json()).expect("valid JSON");

        assert_eq!(json["ok"], false);
        assert_eq!(json["error"]["code"], "E_DENIED");
        assert_eq!(json["error"]["message"], "user denied");
    }

    #[test]
    fn the_schema_list_is_what_a_request_carries() {
        let schemas = schemas();

        assert_eq!(schemas.len(), registry().len());
        for schema in &schemas {
            assert_eq!(schema["type"], "function");
        }
    }

    #[test]
    fn resolved_calls_name_the_same_tools_the_registry_does() {
        // `ResolvedCall::tool` is what dispatch and the audit line both read;
        // if it disagreed with the parser, a call would be logged under one
        // name and run as another.
        let parsed =
            policy::ToolCall::parse(tool::FS_LIST, json!({ "path": "." })).expect("fs_list parses");
        assert_eq!(parsed.tool(), tool::FS_LIST);
    }
}
