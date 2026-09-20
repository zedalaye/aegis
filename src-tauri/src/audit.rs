//! The audit log: one JSON line per tool call in `audit.jsonl`, whatever the
//! outcome (PLAN 2.1 entry shape, PLAN 3.1 decisions).
//!
//! * **Append-only JSONL**: a crash truncates at most the last line, and
//!   unparseable lines are skipped on read.
//! * **Arguments, not content**: paths are kept, file content is replaced by its
//!   size, and the digest covers the full arguments.
//! * **Logging never fails the call**: a write failure is logged loudly.
//! * **Bounded reads** from the end of the file. The runtime also reads it:
//!   [`AuditLog::witnessed`] is the evidence a routine's skill already ran.
//!
//! Fields added later (`agent_id`, `skill`, `handoff`, `routine`) are
//! `#[serde(default)]` so old lines still parse. Tokens are deliberately absent:
//! they are spent per model round, even without tool calls, so cost lives on the
//! session ([`TurnCost`](crate::store::TurnCost)) and joins here on `turn_id`.

use std::fs::{self, OpenOptions};
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use ts_rs::TS;

use crate::error::ErrorCode;

/// Name of the log under the application-data directory.
const AUDIT_FILE: &str = "audit.jsonl";

/// How much of the end of the file [`AuditLog::tail`] reads: tens of thousands
/// of lines.
const TAIL_WINDOW_BYTES: u64 = 4 * 1024 * 1024;

/// The largest `limit` [`AuditLog::tail`] will honour.
const TAIL_MAX: usize = 1000;

/// Longest string value kept verbatim in `args_redacted`.
const REDACT_MAX_CHARS: usize = 96;

/// Argument keys never shortened: paths and programs, where a truncated value
/// reads like a different one.
const KEPT_WHOLE: &[&str] = &["path", "cwd", "program", "display", "artefacts", "from"];

/// Argument keys replaced by their size, so the log never copies file content.
const SIZED_NOT_QUOTED: &[&str] = &["content"];

/// How a tool call came to run, or not (PLAN 2.1, `AuditEntry.decision`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum AuditDecision {
    /// Policy allowed it with no prompt.
    Auto,
    /// The user allowed this one call.
    AllowOnce,
    /// The user allowed it for the rest of the session.
    AllowSession,
    /// The user, or policy, refused it.
    Deny,
    /// A person acted in the window with no model involved — a dropped brief
    /// (PLAN 7.15). No session, identity or turn; boards leave it out.
    Operator,
}

/// How a tool call ended (PLAN 2.1, `AuditEntry.outcome`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum Outcome {
    /// The tool ran and succeeded.
    Ok,
    /// The tool ran and failed on its own terms.
    Error,
    /// It never ran: policy or the user refused it.
    Denied,
    /// It never finished: the turn was cancelled.
    Cancelled,
}

/// A file a tool call left on disk, identified by path, digest and size —
/// never a copy (PLAN 5.4). Only `screen_capture` produces one today.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct AuditArtifact {
    /// Where the file was written.
    pub path: String,
    /// SHA-256 of the bytes on disk, hex.
    pub sha256: String,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// One line of the log, and the `audit_tail` wire format. Later fields default
/// so older lines still parse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct AuditEntry {
    /// RFC3339, UTC, millisecond precision — fixed width, so the file sorts
    /// lexicographically in the order it was written.
    pub ts: String,
    /// Which session made the call.
    pub session_id: String,
    /// Which identity it was made as (Phase 12); empty on older lines.
    #[serde(default)]
    pub agent_id: String,
    /// Which turn within it.
    pub turn_id: String,
    /// The model's own id for the call.
    pub call_id: String,
    /// Tool name.
    pub tool: String,
    /// The skill run this call belongs to (Phase 13; PLAN 7.6): every call
    /// between `skill_run` and `skill_return`. Empty otherwise.
    #[serde(default)]
    pub skill: String,
    /// The delegation this call belongs to (Phase 15): the CoS's
    /// `handoff_delegate` and every call specialists make on its briefs.
    #[serde(default)]
    pub handoff: String,
    /// The routine whose run this call belongs to (Phase 16).
    #[serde(default)]
    pub routine: String,
    /// Auto-allowed, approved, or refused.
    pub decision: AuditDecision,
    /// Why policy decided that, in the words the user was shown.
    pub policy_reason: String,
    /// SHA-256 of the full canonical arguments JSON, hex — including what the
    /// redacted copy shortened.
    pub args_digest: String,
    /// The arguments as JSON, paths kept and values shortened.
    pub args_redacted: String,
    /// How it ended.
    pub outcome: Outcome,
    /// Wall-clock duration of the execution itself.
    #[ts(type = "number")]
    pub duration_ms: u64,
    /// Bytes the call carried in — the content of a write, nothing for a read.
    #[ts(type = "number")]
    pub bytes_in: u64,
    /// Bytes the call produced, before any truncation for the model.
    #[ts(type = "number")]
    pub bytes_out: u64,
    /// The stable code when this failed, `null` otherwise.
    pub error_code: Option<String>,
    /// The file this call wrote, when it wrote one.
    #[serde(default)]
    pub artifact: Option<AuditArtifact>,
}

/// Everything needed to append one line, before the timestamp is taken.
#[derive(Debug)]
pub struct AuditRecord<'a> {
    /// Which session made the call.
    pub session_id: &'a str,
    /// Which identity it was made as.
    pub agent_id: &'a str,
    /// Which turn within it.
    pub turn_id: &'a str,
    /// The model's own id for the call.
    pub call_id: &'a str,
    /// Tool name.
    pub tool: &'a str,
    /// The skill run this call was part of; empty outside one.
    pub skill: &'a str,
    /// The delegation this call was part of; empty outside one.
    pub handoff: &'a str,
    /// The routine whose run this call was part of; empty outside one.
    pub routine: &'a str,
    /// Auto-allowed, approved, or refused.
    pub decision: AuditDecision,
    /// Why, in the words the user was shown.
    pub policy_reason: &'a str,
    /// The arguments as the model sent them. Digested whole, recorded
    /// redacted.
    pub args: &'a serde_json::Value,
    /// How it ended.
    pub outcome: Outcome,
    /// Wall-clock duration of the execution.
    pub duration_ms: u64,
    /// Bytes carried in.
    pub bytes_in: u64,
    /// Bytes produced.
    pub bytes_out: u64,
    /// The stable code when this failed.
    pub error_code: Option<ErrorCode>,
    /// The file the call wrote, when it wrote one.
    pub artifact: Option<AuditArtifact>,
}

impl AuditRecord<'_> {
    /// Stamps the record with the current time and freezes it into an entry.
    fn seal(&self) -> AuditEntry {
        AuditEntry {
            ts: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            session_id: self.session_id.to_owned(),
            agent_id: self.agent_id.to_owned(),
            turn_id: self.turn_id.to_owned(),
            call_id: self.call_id.to_owned(),
            tool: self.tool.to_owned(),
            skill: self.skill.to_owned(),
            handoff: self.handoff.to_owned(),
            routine: self.routine.to_owned(),
            decision: self.decision,
            policy_reason: self.policy_reason.to_owned(),
            args_digest: digest(self.args),
            args_redacted: redact(self.args),
            outcome: self.outcome,
            duration_ms: self.duration_ms,
            bytes_in: self.bytes_in,
            bytes_out: self.bytes_out,
            error_code: self.error_code.map(|code| code.as_str().to_owned()),
            artifact: self.artifact.clone(),
        }
    }
}

/// The append-only log. The mutex serializes writers in this process only;
/// JSONL tolerates other processes interleaving whole lines.
#[derive(Debug)]
pub struct AuditLog {
    path: PathBuf,
    writer: Mutex<()>,
}

impl AuditLog {
    /// A log writing to `audit.jsonl` under `data_dir`, created on first append.
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join(AUDIT_FILE),
            writer: Mutex::new(()),
        }
    }

    /// Where the log lives. Shown in the UI so a user can go and read it.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Locks the writer, recovering from poison: one panic must not stop all
    /// auditing.
    fn writer(&self) -> MutexGuard<'_, ()> {
        self.writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Appends one line and returns the entry, even if the write failed (logged
    /// at `error`), so the UI event still fires.
    pub fn append(&self, record: &AuditRecord<'_>) -> AuditEntry {
        let entry = record.seal();

        if let Err(err) = self.write_line(&entry) {
            tracing::error!(
                %err,
                path = %self.path.display(),
                tool = %entry.tool,
                "a tool call could not be audited"
            );
        }

        entry
    }

    /// Serializes one entry and appends it, newline-terminated.
    fn write_line(&self, entry: &AuditEntry) -> io::Result<()> {
        // Serialized before taking the lock.
        let mut line = serde_json::to_vec(entry).map_err(io::Error::other)?;
        line.push(b'\n');

        let _guard = self.writer();

        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;

        // One write of the whole line, so other appenders cannot split it.
        file.write_all(&line)?;
        file.flush()
    }

    /// The most recent entries, newest first, optionally for one session, with
    /// `limit` clamped to [`TAIL_MAX`]. Reads the last [`TAIL_WINDOW_BYTES`],
    /// dropping a leading fragment; unparseable lines are skipped.
    pub fn tail(&self, limit: usize, session_id: Option<&str>) -> io::Result<Vec<AuditEntry>> {
        let limit = limit.min(TAIL_MAX);
        if limit == 0 {
            return Ok(Vec::new());
        }

        let (bytes, partial_first) = match self.read_tail() {
            Ok(read) => read,
            // No log yet: no tool has run.
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };

        let text = String::from_utf8_lossy(&bytes);
        let mut lines = text.lines();
        if partial_first {
            lines.next();
        }

        let mut out: Vec<AuditEntry> = Vec::new();
        for line in lines {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<AuditEntry>(line) {
                Ok(entry) => {
                    if session_id.is_none_or(|wanted| entry.session_id == wanted) {
                        out.push(entry);
                    }
                }
                Err(err) => tracing::warn!(%err, "skipping an unreadable audit line"),
            }
        }

        // Truncated after filtering, so `limit` counts entries returned.
        out.reverse();
        out.truncate(limit);
        Ok(out)
    }

    /// Whether this identity has carried this skill to a successful
    /// `skill_return`, any status (PLAN 7.13, *Phase 16's door*). Limited to
    /// the tail window: an old run reads as `false`, the safe direction.
    pub fn witnessed(&self, agent_id: &str, skill: &str) -> bool {
        let Ok((bytes, partial_first)) = self.read_tail() else {
            return false;
        };

        let text = String::from_utf8_lossy(&bytes);
        let mut lines = text.lines();
        if partial_first {
            lines.next();
        }

        lines
            .filter_map(|line| serde_json::from_str::<AuditEntry>(line.trim()).ok())
            .any(|entry| {
                entry.tool == crate::policy::tool::SKILL_RETURN
                    && entry.skill == skill
                    && entry.agent_id == agent_id
                    && entry.outcome == Outcome::Ok
            })
    }

    /// Reads at most the last [`TAIL_WINDOW_BYTES`]; the flag is true when the
    /// first line is a fragment.
    fn read_tail(&self) -> io::Result<(Vec<u8>, bool)> {
        let mut file = fs::File::open(&self.path)?;
        let len = file.metadata()?.len();
        let from = len.saturating_sub(TAIL_WINDOW_BYTES);

        if from > 0 {
            file.seek(SeekFrom::Start(from))?;
        }

        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok((bytes, from > 0))
    }
}

/// The key an answer to a parked ask matches (PLAN 7.22): the tool and the
/// digest of exactly the arguments the person read.
///
/// Here rather than in [`policy`](crate::policy) because it must be the same
/// digest the audit line carries: one answer, one line, one call.
pub fn fingerprint(tool: &str, args: &serde_json::Value) -> String {
    format!("{tool}:{}", digest(args))
}

/// SHA-256 of the arguments, hex. `serde_json::Value` objects serialize
/// key-sorted, so key order does not change the digest.
fn digest(args: &serde_json::Value) -> String {
    use std::fmt::Write as _;

    let canonical = serde_json::to_vec(args).unwrap_or_else(|_| b"null".to_vec());
    Sha256::digest(&canonical)
        .iter()
        .fold(String::with_capacity(64), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// The arguments as compact JSON with the same structure: paths kept, long
/// strings shortened, content replaced by its size.
pub(crate) fn redact(args: &serde_json::Value) -> String {
    let redacted = redact_value(None, args);
    serde_json::to_string(&redacted).unwrap_or_else(|_| "\"<unrenderable>\"".to_owned())
}

/// Redacts one value under its key; array elements inherit the array's key.
fn redact_value(key: Option<&str>, value: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;

    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(field, value)| (field.clone(), redact_value(Some(field), value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| redact_value(key, item))
                .collect::<Vec<_>>(),
        ),
        Value::String(text) => Value::String(redact_string(key, text)),
        other => other.clone(),
    }
}

/// Shortens one string according to the key it was found under.
fn redact_string(key: Option<&str>, text: &str) -> String {
    if key.is_some_and(|key| SIZED_NOT_QUOTED.contains(&key)) {
        return format!("<{} bytes>", text.len());
    }
    if key.is_some_and(|key| KEPT_WHOLE.contains(&key)) {
        return text.to_owned();
    }

    let mut kept: String = text.chars().take(REDACT_MAX_CHARS).collect();
    if kept.chars().count() < text.chars().count() {
        kept.push('…');
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;
    use tempfile::TempDir;

    fn record<'a>(args: &'a serde_json::Value, session: &'a str) -> AuditRecord<'a> {
        AuditRecord {
            agent_id: "default",
            session_id: session,
            turn_id: "t1",
            call_id: "call_1",
            tool: "fs_read",
            skill: "",
            handoff: "",
            routine: "",
            decision: AuditDecision::Auto,
            policy_reason: "an ordinary read inside the workspace",
            args,
            outcome: Outcome::Ok,
            duration_ms: 3,
            bytes_in: 0,
            bytes_out: 12,
            error_code: None,
            artifact: None,
        }
    }

    #[test]
    fn the_digest_ignores_key_order() {
        let one = json!({ "path": "a.txt", "limit": 10 });
        let two = json!({ "limit": 10, "path": "a.txt" });

        assert_eq!(digest(&one), digest(&two));
        assert_eq!(digest(&one).len(), 64);
    }

    #[test]
    fn a_written_file_never_reaches_the_log() {
        let secret = "sk-live-0123456789";
        let args = json!({ "path": ".env", "content": secret });

        let redacted = redact(&args);

        assert!(
            redacted.contains(".env"),
            "the path is the point: {redacted}"
        );
        assert!(
            !redacted.contains(secret),
            "content must never be quoted: {redacted}"
        );
        assert!(redacted.contains("<18 bytes>"), "{redacted}");
    }

    #[test]
    fn long_values_are_shortened_but_paths_are_not() {
        let long_path = format!("src/{}/main.rs", "deeply_nested".repeat(20));
        let args = json!({ "path": long_path, "note": "x".repeat(200) });

        let redacted = redact(&args);

        assert!(redacted.contains(&long_path), "a cut path names nothing");
        assert!(!redacted.contains(&"x".repeat(200)));
        assert!(redacted.contains('…'));
    }

    #[test]
    fn appending_writes_one_terminated_line_per_call() {
        let dir = TempDir::new().expect("temp dir");
        let log = AuditLog::new(dir.path());
        let args = json!({ "path": "a.txt" });

        log.append(&record(&args, "s1"));
        log.append(&record(&args, "s1"));

        let text = fs::read_to_string(log.path()).expect("the log exists");
        assert_eq!(text.lines().count(), 2);
        assert!(text.ends_with('\n'), "every line is terminated");
    }

    #[test]
    fn a_torn_last_line_does_not_hide_the_good_ones() {
        let dir = TempDir::new().expect("temp dir");
        let log = AuditLog::new(dir.path());
        let args = json!({ "path": "a.txt" });
        log.append(&record(&args, "s1"));

        let mut file = OpenOptions::new()
            .append(true)
            .open(log.path())
            .expect("open the log");
        file.write_all(b"{\"ts\":\"2026").expect("write a fragment");

        let entries = log.tail(10, None).expect("tail");
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn tailing_a_log_that_does_not_exist_yet_is_empty() {
        let dir = TempDir::new().expect("temp dir");
        let log = AuditLog::new(&dir.path().join("nothing"));

        assert!(log.tail(10, None).expect("tail").is_empty());
    }
}
