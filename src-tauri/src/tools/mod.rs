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
//! Tools land with their phases. `fs_list`, `fs_read` and `fs_write` are here
//! now; `shell_exec` (Phase 7) and `screen_capture` (Phase 9) have policy rows
//! already but no implementation, and [`run`] answers a call for one with an
//! honest "not in this build" envelope rather than pretending. They are absent
//! from [`schemas`] for the same reason: a model should not be offered a tool
//! that cannot run.

pub mod fs;

use std::time::Instant;

use serde::{Serialize, Serializer};
use serde_json::{json, Value};

use crate::audit::{AuditDecision, AuditEntry, AuditLog, AuditRecord, Outcome};
use crate::error::ErrorCode;
use crate::policy::{tool, ResolvedCall};

/// Most bytes `fs_read` will return in one envelope (PLAN 4.3).
pub const READ_MAX_BYTES: u64 = 256 * 1024;

/// Most entries `fs_list` will return in one envelope (PLAN 4.3).
pub const LIST_MAX_ENTRIES: u32 = 1000;

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
        }
    }

    /// Records what the call carried in — a write's content, and nothing else
    /// in this phase.
    pub(crate) const fn with_bytes_in(mut self, bytes: u64) -> Self {
        self.bytes_in = bytes;
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
    ]
}

/// The `tools` array for a model request (PLAN 4.1).
pub fn schemas() -> Vec<Value> {
    registry().iter().map(ToolSpec::to_schema).collect()
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
#[derive(Debug, Clone, Copy)]
pub struct ToolCtx<'a> {
    /// Which session made the call. Every audit line carries it.
    pub session_id: &'a str,
    /// Which turn within the session.
    pub turn_id: &'a str,
    /// The model's own id for this call.
    pub call_id: &'a str,
    /// Where the audit line goes.
    pub audit: &'a AuditLog,
    /// The arguments as the model sent them, for the digest and the redacted
    /// copy.
    pub args: &'a Value,
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
pub fn run(
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
        // Policy has a row for these from Phase 3, and the model is not
        // offered them until they exist. A model that names one anyway gets an
        // answer it can act on rather than a silent nothing.
        ResolvedCall::ShellExec { .. } | ResolvedCall::ScreenCapture { .. } => Produced::failed(
            name,
            ErrorCode::ToolFailed,
            format!("`{name}` is not available in this build"),
        ),
    };

    // `as` saturates at `u64::MAX` here, which is 584 million years: the cast
    // cannot be the thing that goes wrong in this line.
    #[allow(clippy::cast_possible_truncation)]
    let duration_ms = started.elapsed().as_millis() as u64;

    let audit = ctx.audit.append(&AuditRecord {
        session_id: ctx.session_id,
        turn_id: ctx.turn_id,
        call_id: ctx.call_id,
        tool: name,
        decision,
        policy_reason: reason,
        args: ctx.args,
        outcome: if produced.result.ok {
            Outcome::Ok
        } else {
            Outcome::Error
        },
        duration_ms,
        bytes_in: produced.bytes_in,
        bytes_out: produced.bytes_out,
        error_code: produced.result.error.as_ref().map(|error| error.code),
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
        turn_id: ctx.turn_id,
        call_id: ctx.call_id,
        tool: tool_name,
        decision,
        policy_reason: reason,
        args: ctx.args,
        outcome: Outcome::Denied,
        duration_ms: 0,
        bytes_in: 0,
        bytes_out: 0,
        error_code: Some(code),
    });

    tracing::info!(tool = tool_name, code = %code, "tool call refused");

    ToolOutcome {
        result: ToolResult::failure(tool_name, code, reason),
        summary: reason.to_owned(),
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

    #[test]
    fn tools_not_in_this_build_are_not_offered_to_the_model() {
        let offered: Vec<&str> = registry().iter().map(|spec| spec.name).collect();

        assert!(!offered.contains(&tool::SHELL_EXEC), "Phase 7");
        assert!(!offered.contains(&tool::SCREEN_CAPTURE), "Phase 9");
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
