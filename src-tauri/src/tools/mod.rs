//! The tool registry: declaration, schema export, dispatch and the envelope.
//!
//! ```text
//! model args ──▶ policy::decide ──▶ ResolvedCall ──▶ tools::run ──▶ ToolResult
//!                      │                                  │
//!                      └── AskRequest                     └── one audit line
//! ```
//!
//! A tool is declared once, as a [`ToolSpec`] whose name is the constant the
//! policy table and the audit log key on (PLAN 4.1), so nothing can exist that
//! nothing gates. Tools take a [`ResolvedCall`], never the model's strings, and
//! every exit from [`run`] writes exactly one audit line. The envelope is one
//! shape for success and failure (PLAN 4.3); a denial is an ordinary result.
//!
//! Connector tools (Phase 18) are not declared here but join the same `tools`
//! array ([`schemas_for`]), the same policy and the same audit line: there is no
//! built-in-versus-MCP branch (PLAN 7.1).
//!
//! [`run`] is `async` for the calls that leave the process or wait: a command
//! (awaited inside the turn's cancellation), a capture (on a blocking thread),
//! a delegation and a connector call.

pub mod connector;
pub mod fs;
pub mod handoff;
pub mod memory;
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
use crate::mcp::{self, Connectors};
use crate::policy::{tool, ResolvedCall};
use crate::skills::SkillCtx;
use crate::store::memories::MemoryStore;
use handoff::HandoffCtx;

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

/// Where a running tool's output goes while it runs. Only `shell_exec` produces
/// any. The tool hands over text; the event stream numbers it.
pub trait ProgressSink: Send + Sync {
    /// Delivers one frame of output. Never fails: a missed frame is cosmetic.
    fn chunk(&self, stream: Stream, text: &str);
}

/// A [`ProgressSink`] that drops everything, for calls nobody watches.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullProgress;

impl ProgressSink for NullProgress {
    fn chunk(&self, _stream: Stream, _text: &str) {}
}

// ---------------------------------------------------------------------------
// The envelope
// ---------------------------------------------------------------------------

/// What the model sees for every tool call and outcome (PLAN 4.3): one shape,
/// with `error` present exactly when `ok` is false.
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
    /// The size before truncation, as each tool defines it.
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

    /// A failure, with empty `content` so the model reads the error.
    /// [`ToolResult::refusal`] is the public name.
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

    /// An envelope for a call that never reached [`run`]: arguments that did
    /// not parse, a call abandoned by a cancel.
    pub fn refusal(tool: &str, code: ErrorCode, message: impl Into<String>) -> Self {
        Self::failure(tool, code, message)
    }

    /// An envelope built directly, for tests of the runtime around tools.
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
            // Unreachable for this shape; the model still gets valid JSON.
            r#"{"ok":false,"error":{"code":"E_TOOL_FAILED","message":"the result could not be rendered"}}"#
                .to_owned()
        })
    }
}

/// What one tool call produced: the envelope for the model, plus the summary
/// and audit line the runtime needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutcome {
    /// The envelope for the model.
    pub result: ToolResult,
    /// One human line: `read src/main.rs (2.4 KB)`. Never a raw blob.
    pub summary: String,
    /// A capture's path, for the transcript. A path, not bytes: the WebView
    /// loads it through the scoped asset protocol (PLAN 5.4).
    pub image_path: Option<String>,
    /// The audit line that was written for this call.
    pub audit: AuditEntry,
}

/// What a tool produced, before the runtime adds the session, audit and timing.
pub(crate) struct Produced {
    /// The envelope.
    result: ToolResult,
    /// The one-line summary.
    summary: String,
    /// Bytes the call carried in.
    bytes_in: u64,
    /// Bytes the call produced.
    bytes_out: u64,
    /// The audit outcome when `ok` does not say it: a cancelled command.
    outcome: Option<Outcome>,
    /// The file the call left on disk (a capture).
    artifact: Option<AuditArtifact>,
    /// The skill run this call opened. Set by `skill_run`, whose own line would
    /// otherwise carry no name; later calls take it from [`ToolCtx::skills`].
    skill: Option<String>,
    /// The delegation this call opened, set by `handoff_delegate` for the same
    /// reason.
    handoff: Option<String>,
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
            handoff: None,
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
            handoff: None,
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

    /// Names the delegation this call opened.
    pub(crate) fn in_handoff(mut self, id: &str) -> Self {
        self.handoff = Some(id.to_owned());
        self
    }
}

// ---------------------------------------------------------------------------
// The registry
// ---------------------------------------------------------------------------

/// One tool, declared once. The schema forbids unknown keys, while
/// [`ToolCall::parse`](crate::policy::ToolCall::parse) ignores them: strict in
/// the contract, forgiving at the door.
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

/// Every tool this build can run, in the order the model sees them: look,
/// read, then change something.
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
            description: "Capture the primary display and write it to a PNG outside the \
                          workspace. Returns the file's path, its pixel size and a SHA-256 — \
                          never the image, which this build cannot read back, so do not expect \
                          to see what was on the screen. Every capture is approved by the user \
                          first and holds whatever was on that display, so ask for one only \
                          when the user has asked to be looked at.",
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
                          `done`, `blocked` or `needs_you`; `summary` is five lines at most. A \
                          `done` has to point at something checkable: a path in `artefacts`, or \
                          what you read or ran, in `evidence`. Artefact paths are verified, so a \
                          `done` naming a file that is not on disk is refused — and so is one \
                          pointing at nothing at all. A `blocked` or a `needs_you` needs at \
                          least one `open_questions` entry.",
            parameters: skill::return_schema,
        },
        ToolSpec {
            name: tool::MEMORY_WRITE,
            description: "Remember one thing as this identity, for every later session. Use it \
                          for a preference, an exception or a convention — something that will \
                          still be true next month and that you would otherwise have to be told \
                          again. A fact about the project belongs in a workspace file instead, \
                          and a procedure belongs in a skill. The user approves each one and can \
                          correct or delete it afterwards; you cannot.",
            parameters: memory::write_schema,
        },
        ToolSpec {
            name: tool::MEMORY_SEARCH,
            description: "Look through what this identity remembers. Your instructions already \
                          carry the most recent memories, so use this for older ones, or to \
                          check whether something is already known before remembering it again.",
            parameters: memory::search_schema,
        },
        ToolSpec {
            name: tool::HANDOFF_DELEGATE,
            description: "Hand briefs to other identities and wait for what they return. Each \
                          brief runs at the same time, as its owner, in a session of its own, \
                          with its own tools and its own memory — none of yours. Inputs are \
                          paths and links, never pasted text: write the document first and name \
                          it. What comes back is a board of statuses and artefact paths, not \
                          their conversations, and an owner that does not answer twice comes \
                          back as `needs_you` for the human.",
            parameters: handoff::delegate_schema,
        },
        ToolSpec {
            name: tool::HANDOFF_RETURN,
            description: "Close the brief you were given and report what came of it. Only a \
                          delegated run has one to close. `status` is `done`, `blocked` or \
                          `needs_you`; `summary` is five lines at most. A `done` has to point at \
                          something checkable: a path in `artefacts`, or — when the brief asked \
                          only for a status — what you read or ran, in `evidence`. Artefact \
                          paths are verified, so a `done` naming a file that is not on disk is \
                          refused, and so is one pointing at nothing at all. A `blocked` or a \
                          `needs_you` needs at least one `open_questions` entry.",
            parameters: handoff::report_schema,
        },
    ]
}

/// The `tools` array for a model request (PLAN 4.1), for a build with no
/// connectors.
pub fn schemas() -> Vec<Value> {
    registry().iter().map(ToolSpec::to_schema).collect()
}

/// The `tools` array narrowed to one identity's allow-list (PLAN 7.3,
/// Phase 12): registry order, then the connectors' tools, in one array. The
/// model is never shown a tool it may not call, and policy still refuses one
/// ([`decide_call`](crate::policy::decide_call)).
pub fn schemas_for(allowed: &[String], catalog: &mcp::Catalog) -> Vec<Value> {
    let mut schemas: Vec<Value> = registry()
        .iter()
        .filter(|spec| allowed.iter().any(|name| name == spec.name))
        .map(ToolSpec::to_schema)
        .collect();
    schemas.extend(catalog.schemas_for(allowed));
    schemas
}

/// Every tool name this build can run: the vocabulary allow-lists are validated
/// against ([`store::agents`](crate::store::agents)).
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

/// Everything a tool call needs from the runtime. `args` is what the model
/// sent, kept so the audit line records the call as it was made.
#[derive(Clone, Copy)]
pub struct ToolCtx<'a> {
    /// Which session made the call. Every audit line carries it.
    pub session_id: &'a str,
    /// Which identity it was made as, for the log; policy already checked the
    /// allow-list.
    pub agent_id: &'a str,
    /// Which turn within the session.
    pub turn_id: &'a str,
    /// The model's own id for this call.
    pub call_id: &'a str,
    /// Where the audit line goes.
    pub audit: &'a AuditLog,
    /// Where captures are written: a fact about the installation, not the call.
    pub captures: &'a Path,
    /// The arguments as the model sent them, for the digest and the redacted
    /// copy.
    pub args: &'a Value,
    /// Where a running tool's output goes while it is still running.
    pub progress: &'a dyn ProgressSink,
    /// The turn's cancellation token, so Stop ends a running command.
    pub cancel: &'a CancellationToken,
    /// The skill library and the open run (Phase 13). `active` puts the
    /// skill's name on every audit line of a run (PLAN 7.6, *Audit names the
    /// skill*).
    pub skills: SkillCtx<'a>,
    /// The memory store (Phase 14). Which identity remembers comes only from
    /// [`ToolCtx::agent_id`].
    pub memories: &'a MemoryStore,
    /// The handoff bus and the brief this turn answers (Phase 15). `open` puts
    /// the delegation id on every audit line a specialist writes (PLAN 7.2,
    /// row 10).
    pub handoffs: HandoffCtx<'a>,
    /// The running connectors (Phase 18). Calls reach this roster; policy
    /// judged a snapshot taken at the top of the turn.
    pub connectors: &'a Connectors,
    /// The routine whose run this is, or empty (Phase 16), put on every audit
    /// line of the run.
    pub routine: &'a str,
}

// By hand: `&dyn ProgressSink` has no `Debug`.
impl fmt::Debug for ToolCtx<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolCtx")
            .field("session_id", &self.session_id)
            .field("agent_id", &self.agent_id)
            .field("turn_id", &self.turn_id)
            .field("call_id", &self.call_id)
            .field("captures", &self.captures)
            .field("skill", &self.skills.active)
            .field("handoff", &self.handoffs.id())
            .field("routine", &self.routine)
            .field("cancelled", &self.cancel.is_cancelled())
            .finish_non_exhaustive()
    }
}

/// A refusal when a resolved path no longer resolves to itself
/// ([`path::unchanged`](crate::policy::path::unchanged)), checked right before
/// the tool acts.
fn moved(tool_name: &str, path: &Path) -> Option<Produced> {
    if crate::policy::path::unchanged(path) {
        return None;
    }
    tracing::warn!(
        tool = tool_name,
        "a resolved path changed on disk between the decision and the call"
    );
    Some(Produced::failed(
        tool_name,
        ErrorCode::PathInvalid,
        format!(
            "`{}` changed on disk after this call was decided: a folder on the way is now a link, \
             or was replaced. Nothing was done. Make the call again to have it judged as it is now",
            path.display()
        ),
    ))
}

/// Runs a call that policy or the user cleared, and audits it — the only way a
/// tool runs. `decision` and `reason` record why it ran. A failure is an
/// envelope, never an `Err` (PLAN 4.3).
pub async fn run(
    ctx: &ToolCtx<'_>,
    decision: AuditDecision,
    reason: &str,
    call: &ResolvedCall,
) -> ToolOutcome {
    let name = call.tool();
    let started = Instant::now();

    let produced = match call {
        ResolvedCall::FsList { path, max_entries } => {
            moved(name, path).unwrap_or_else(|| fs::list(path, *max_entries))
        }
        ResolvedCall::FsRead {
            path,
            offset,
            limit,
        } => moved(name, path).unwrap_or_else(|| fs::read(path, *offset, *limit)),
        ResolvedCall::FsWrite {
            path,
            content,
            create_dirs,
        } => moved(name, path).unwrap_or_else(|| fs::write(path, content, *create_dirs)),
        ResolvedCall::ShellExec {
            program,
            args,
            cwd,
            host,
            timeout_ms,
        } => match moved(name, cwd) {
            Some(refused) => refused,
            None => {
                shell::exec(
                    program,
                    args,
                    cwd,
                    host.as_deref(),
                    *timeout_ms,
                    ctx.progress,
                    ctx.cancel,
                )
                .await
            }
        },
        // On a blocking thread: a round trip to the window server, possibly
        // through a consent prompt.
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
        // Inside the process, inline.
        ResolvedCall::SkillRun { name } => skill::run(name, ctx.skills),
        ResolvedCall::SkillReturn { report } => skill::ret(report, ctx.skills),
        ResolvedCall::MemoryWrite { kind, text, source } => {
            memory::write(ctx.memories, ctx.agent_id, *kind, text, source.as_deref())
        }
        ResolvedCall::MemorySearch { query } => memory::search(ctx.memories, ctx.agent_id, query),
        // Awaited: the model asked a question. The bus bounds the wait.
        ResolvedCall::HandoffDelegate { plan } => {
            handoff::delegate(plan, ctx.handoffs, ctx.cancel).await
        }
        ResolvedCall::HandoffReturn { report } => handoff::ret(report, ctx.handoffs),
        ResolvedCall::Connector { name, args } => {
            connector::call(ctx.connectors, name, args, ctx.cancel).await
        }
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
        // The call's own claim first (see `Produced::skill`), then the open run.
        skill: produced
            .skill
            .as_deref()
            .or(ctx.skills.active)
            .unwrap_or(""),
        // Likewise for a delegation.
        handoff: produced
            .handoff
            .as_deref()
            .unwrap_or_else(|| ctx.handoffs.id()),
        routine: ctx.routine,
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

/// Records a call that never ran and returns its envelope: a policy hard denial
/// (PLAN 3.2, with policy's code) or a user's `deny`. One path, told apart in
/// the log by `decision`.
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
        // A refusal inside a run or a brief belongs to it.
        skill: ctx.skills.active.unwrap_or(""),
        handoff: ctx.handoffs.id(),
        routine: ctx.routine,
        decision,
        policy_reason: reason,
        args: ctx.args,
        outcome: Outcome::Denied,
        duration_ms: 0,
        bytes_in: 0,
        bytes_out: 0,
        error_code: Some(code),
        // A call that never ran wrote nothing.
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
            tool::MEMORY_WRITE,
            tool::MEMORY_SEARCH,
            tool::HANDOFF_DELEGATE,
            tool::HANDOFF_RETURN,
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
