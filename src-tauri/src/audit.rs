//! The audit log.
//!
//! One JSON line per tool call, appended to `audit.jsonl` under the
//! application-data directory, whatever the outcome — allowed, refused,
//! failed, cancelled. This is the record that makes the approval gate
//! meaningful after the fact: an approval the user has already clicked through
//! is only worth something if there is a way to go back and read what was
//! actually done.
//!
//! Four properties shape the format:
//!
//! * **One line per call, append-only.** JSONL rather than a JSON document, so
//!   a line can be written without reading what came before, a crash truncates
//!   at most the last line, and `tail -f` works. A line that will not parse is
//!   skipped on read rather than poisoning the file.
//! * **Arguments are recorded, content is not.** The digest is over the
//!   arguments as the model sent them; the human-readable copy keeps the paths
//!   — which are the point — and replaces file content with its size. A log
//!   that quoted the bytes of every `fs_write` would become the one place on
//!   the machine where every secret the agent ever wrote is collected in plain
//!   text.
//! * **A failure to log never fails the call.** The line is written after the
//!   tool has already run, so refusing at that point would be theatre; an
//!   unwritable log is loud in the tracing output, and the entry still reaches
//!   the UI.
//! * **Reading is bounded.** [`AuditLog::tail`] reads from the end of the
//!   file, so a log that has grown for months still answers instantly. It is
//!   also read *by the runtime* from Phase 16: a routine may only name a skill
//!   its identity has already carried to a `skill_return`, and this is where
//!   that evidence lives (`AuditLog::witnessed`).
//!
//! The entry shape is PLAN 2.1, "Settings and audit"; the decision vocabulary
//! is PLAN 3.1's. It has grown four times since, every time by adding a field
//! with a `serde` default rather than by changing one — `agent_id` in Phase 12,
//! `skill` in Phase 13, `handoff` in Phase 15 and `routine` in Phase 16 — which
//! is the property PLAN 7.1 asks the log to keep: a schema that can grow
//! `agent_id`, `skill`, `tokens`, `handoff_id`.
//!
//! Four of those five ids are here. **`tokens` is not, and will not be.** Phase
//! 17 is the phase that would have added it, and adding it would have been a
//! lie: tokens are spent by a model round, not by a tool call, several calls
//! come out of one round, and — the fact that settles it — a turn that called
//! no tool at all still spends them. A counter built from this file would
//! silently omit every reply that only talked. So cost is recorded on the
//! session, per turn ([`TurnCost`](crate::store::TurnCost)), and joined to this
//! file on the `turn_id` every line has carried since Phase 4. The extensible
//! schema did its job; the field it was extended with belonged somewhere else.

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

/// How much of the tail of the file [`AuditLog::tail`] will read.
///
/// A tool call's line is a few hundred bytes, so this is tens of thousands of
/// calls — far more than any `limit` the UI asks for, and a fixed ceiling on
/// what a log left running for a year costs to read.
const TAIL_WINDOW_BYTES: u64 = 4 * 1024 * 1024;

/// The largest `limit` [`AuditLog::tail`] will honour.
const TAIL_MAX: usize = 1000;

/// Longest string value kept verbatim in `args_redacted`.
const REDACT_MAX_CHARS: usize = 96;

/// Argument keys whose value is the point of the record and is never
/// shortened.
///
/// These are what a person scanning the log is looking for: *which file*,
/// *which folder*, *which program*. They are short by nature, and a truncated
/// path is worse than useless — it reads like a different path.
///
/// `artefacts` joined them in Phase 17, and for exactly that reason: a report
/// names what the work produced, a trace reads those names back off the line to
/// say what a run left on disk, and a path cut at ninety-six characters names
/// nothing. It is a list of paths under a different key, not a new kind of
/// value.
///
/// `from` joined in PLAN 7.15 for the same reason: a brief dropped onto the
/// project records where it was copied from, and that is a path.
const KEPT_WHOLE: &[&str] = &["path", "cwd", "program", "display", "artefacts", "from"];

/// Argument keys replaced by their size rather than recorded.
///
/// `fs_write.content` is the whole reason this list exists: it is the one
/// argument that routinely carries the contents of a file, and the audit log
/// must not become a copy of everything the agent has ever written.
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
    /// A person did it themselves, in the window, with no model involved
    /// (PLAN 7.15).
    ///
    /// Not an approval: nothing asked, and no gate stood between the act and
    /// the file. It exists for the one such act that writes into a workspace —
    /// a file dropped onto the project and copied in as a brief — so that intake
    /// no session wrote is still on the record. Such a line carries no session,
    /// identity or turn, and a board fold, which is scoped by session, leaves
    /// it out.
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

/// A file a tool call left on disk, identified without the log holding a copy.
///
/// `screen_capture` is the only tool that produces one today, and PLAN 5.4
/// names exactly what its line may carry: the path, the dimensions and a
/// digest, never the image. That is enough to say afterwards *which* capture a
/// call produced, and to check that the file still on disk is the one this line
/// is about.
///
/// The dimensions are pixels because the only artefact so far is an image; a
/// later tool that writes something else will widen this shape rather than
/// borrow it, and the field being optional is what lets it (PLAN 7.1: the audit
/// schema has to be able to grow).
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

/// One line of the log.
///
/// Serialized and deserialized with the same struct on purpose: the file *is*
/// the wire format for `audit_tail`, so a field the writer adds is a field the
/// reader sees, and there is no second shape to keep in step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct AuditEntry {
    /// RFC3339, UTC, millisecond precision — fixed width, so the file sorts
    /// lexicographically in the order it was written.
    pub ts: String,
    /// Which session made the call.
    pub session_id: String,
    /// Which identity it was made as (PLAN 7.3, Phase 12).
    ///
    /// The first half of "who ran, what did it cost, why did it fail" (PLAN
    /// 7.2, row 10). A tool call is gated on the identity's allow-list, so a
    /// record of the call that does not name the identity cannot be read back
    /// against the grant that let it through.
    ///
    /// `#[serde(default)]` for the lines written before identities existed: the
    /// file is its own wire format, and a reader that refused those lines would
    /// lose the history the log is kept for. Those lines carry an empty string,
    /// which is not an identity and is drawn as none.
    #[serde(default)]
    pub agent_id: String,
    /// Which turn within it.
    pub turn_id: String,
    /// The model's own id for the call.
    pub call_id: String,
    /// Tool name.
    pub tool: String,
    /// The skill run this call was part of (PLAN 7.3, Phase 13).
    ///
    /// "A run without `skill` on the line cannot be budgeted or replayed"
    /// (PLAN 7.6). Every call made between a `skill_run` and its
    /// `skill_return` carries the name — not only the two the skill tools make
    /// — so the question a replay asks is answerable: *what did this runbook
    /// actually do, and what was it refused*.
    ///
    /// Empty for a call made outside a run, and for every line written before
    /// this phase. `#[serde(default)]` for the same reason `agent_id` carries
    /// one: the file is its own wire format, and a reader that refused the
    /// older lines would lose the history the log is kept for.
    #[serde(default)]
    pub skill: String,
    /// The delegation this call was part of (PLAN 7.3, Phase 15).
    ///
    /// The other half of "one run id over CoS + specialists" (PLAN 7.2, row
    /// 10): the CoS's `handoff_delegate` line carries it, and so does every
    /// call every specialist makes while working on one of its briefs — in
    /// their own sessions, under their own identities. Given `agent_id` beside
    /// it, a replay can say who ran, under whose brief, and what it cost.
    ///
    /// Empty outside a delegation, and on every line written before this
    /// phase. `#[serde(default)]` for the reason `agent_id` and `skill` carry
    /// one: the file is its own wire format, and a reader that refused the
    /// older lines would lose the history the log is kept for.
    #[serde(default)]
    pub handoff: String,
    /// The routine whose run this call was part of (PLAN 7.3, Phase 16).
    ///
    /// The third id over a run, beside `agent_id` and `skill`, and the one that
    /// answers a question only this phase can raise: *what did the machine do
    /// while nobody was here*. Every call a scheduled run makes carries it, so
    /// a week of a watch routine is one grep — and so is the budget it spent,
    /// which is the "cannot be budgeted or replayed" of `COS.md` applied to the
    /// only runs nobody watched.
    ///
    /// Empty outside a routine's run, and on every line written before this
    /// phase. `#[serde(default)]` for the reason the three before it carry one:
    /// the file is its own wire format, and a reader that refused the older
    /// lines would lose the history the log is kept for.
    #[serde(default)]
    pub routine: String,
    /// Auto-allowed, approved, or refused.
    pub decision: AuditDecision,
    /// Why policy decided that, in the words the user was shown.
    pub policy_reason: String,
    /// SHA-256 of the canonical arguments JSON, hex.
    ///
    /// The digest is over the arguments in full, including anything the
    /// redacted copy shortened, so two calls can be compared for identity even
    /// though neither line quotes what they carried.
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
    ///
    /// `#[serde(default)]` because the file is its own wire format: a log
    /// written by an earlier build has no such key, and a reader that refused
    /// those lines would lose the history the log exists to keep.
    #[serde(default)]
    pub artifact: Option<AuditArtifact>,
}

/// Everything needed to append one line, before the timestamp is taken.
///
/// A struct rather than a dozen positional arguments: the fields are almost
/// all strings and numbers, and a call site that transposed two of them would
/// still compile.
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

/// The append-only log.
///
/// The mutex serializes writers within the process. It is not a lock on the
/// file: another process appending to the same log would interleave, which is
/// exactly what JSONL tolerates and a JSON document would not.
#[derive(Debug)]
pub struct AuditLog {
    path: PathBuf,
    writer: Mutex<()>,
}

impl AuditLog {
    /// A log writing to `audit.jsonl` under `data_dir`.
    ///
    /// Nothing is opened or created here: a run in which no tool call happens
    /// should not leave an empty file behind, so the first append creates it.
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

    /// Locks the writer.
    ///
    /// Poisoning is recovered rather than propagated: what the guard protects
    /// is the ordering of appends to a file, not an invariant in memory, so a
    /// panic elsewhere leaves nothing here torn. Refusing to log afterwards
    /// would turn one panic into a permanently unaudited runtime.
    fn writer(&self) -> MutexGuard<'_, ()> {
        self.writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Appends one line and returns the entry that was written.
    ///
    /// The entry comes back whether or not the write succeeded, because the
    /// caller has an `audit:appended` event to emit and a UI to update either
    /// way. A write failure is logged at `error` — it is a real problem, and
    /// the log is the one place that cannot report its own absence.
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
        // Serialized before the lock is taken: it cannot fail for this shape,
        // but holding the writer across work that does not need it is how a
        // log becomes a bottleneck.
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

        // One `write_all` of a whole line: append mode makes a single write
        // atomic against other appenders, so a concurrent writer — another
        // Aegis process on the same profile — cannot land halfway through a
        // line.
        file.write_all(&line)?;
        file.flush()
    }

    /// The most recent entries, newest first.
    ///
    /// `session_id` filters to one session; `limit` is clamped to
    /// [`TAIL_MAX`]. Only the last [`TAIL_WINDOW_BYTES`] of the file are read,
    /// and the first line in that window is dropped unless the window starts
    /// at the beginning of the file — it is almost certainly a fragment, and a
    /// fragment is not an entry.
    ///
    /// A line that does not parse is skipped rather than failing the call: the
    /// last line of a log whose process was killed mid-write is exactly that
    /// case, and it must not hide the thousands of good lines above it.
    pub fn tail(&self, limit: usize, session_id: Option<&str>) -> io::Result<Vec<AuditEntry>> {
        let limit = limit.min(TAIL_MAX);
        if limit == 0 {
            return Ok(Vec::new());
        }

        let (bytes, partial_first) = match self.read_tail() {
            Ok(read) => read,
            // No log yet means no tool has run yet, which is an empty list
            // rather than an error the UI has to explain.
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

        // Newest first, and only as many as were asked for. Truncating after
        // the filter is what makes `limit` mean "entries you will see" rather
        // than "lines I happened to look at".
        out.reverse();
        out.truncate(limit);
        Ok(out)
    }

    /// Whether this identity has already carried this skill to a
    /// `skill_return` (PLAN 7.13, *Phase 16's door*).
    ///
    /// The one place the runtime *reads* its own log to decide something, and
    /// it is deliberate: "run it under watch, then put it on a clock" is a rule
    /// about something that has already happened, and the log is where what has
    /// happened is written down. It is also the payoff for PLAN 7.6's *audit
    /// names the skill* — without the name on the line there would be no way to
    /// ask this question at all.
    ///
    /// A **return** rather than a run, because that is the half that means the
    /// runbook reached its end: a `skill_run` says somebody opened the file.
    /// Any status counts — a `blocked` is a runbook doing its job.
    ///
    /// Bounded by [`TAIL_WINDOW_BYTES`] like every other read of this file, so
    /// a run from long enough ago has scrolled out of the window and the answer
    /// is `false`. That is the safe direction, and the fix is to run it once
    /// more and watch it, which is the thing the rule is asking for anyway.
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

    /// Reads at most the last [`TAIL_WINDOW_BYTES`] of the log.
    ///
    /// The flag says whether the window started mid-file, and therefore
    /// whether its first line is a fragment.
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

/// SHA-256 of the arguments, hex, over their canonical JSON form.
///
/// `serde_json::Value` keeps object keys in a `BTreeMap`, so serializing one
/// is already key-sorted and whitespace-free: two calls with the same
/// arguments written in a different key order digest identically, which is
/// what makes the digest usable for spotting a repeated call.
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

/// The arguments as a compact JSON string, with paths kept and values
/// shortened.
///
/// Structure is preserved — the keys, the nesting and the types are all still
/// there — so the line reads as the call that was made. What changes is that a
/// value which could be arbitrarily long, or could be a secret, is replaced by
/// a description of itself.
pub(crate) fn redact(args: &serde_json::Value) -> String {
    let redacted = redact_value(None, args);
    serde_json::to_string(&redacted).unwrap_or_else(|_| "\"<unrenderable>\"".to_owned())
}

/// Redacts one value, given the key it was found under.
///
/// An array inherits its key, so `shell_exec`'s `args: [...]` is shortened
/// element by element under the same rule as any other value.
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
        // Numbers, booleans and null are already as short as they will ever
        // be, and none of them can hide a file's contents.
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
