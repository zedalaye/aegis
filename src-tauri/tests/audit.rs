//! The audit log as a file on disk.
//!
//! `tests/tools_fs.rs` checks that tool calls produce lines. This file checks
//! the properties of the log itself — the ones a person auditing a machine
//! actually depends on:
//!
//! * every call leaves exactly one line, in the order the calls were made;
//! * the line carries the documented field names, because the file *is* the
//!   wire format `audit_tail` returns and a renamed field is a broken UI;
//! * a file's contents never appear in it, whatever a tool was asked to write;
//! * reading survives a log that a killed process left half-written, and one
//!   that has grown past anything a reader wants in memory.

use std::fs::{self, OpenOptions};
use std::io::Write as _;

use aegis_lib::audit::{AuditDecision, AuditLog, AuditRecord, Outcome};
use aegis_lib::{ErrorCode, DEFAULT_AGENT_ID};
use serde_json::{json, Value};
use tempfile::TempDir;

/// A log in a fresh directory.
struct Fixture {
    _guard: TempDir,
    log: AuditLog,
}

impl Fixture {
    fn new() -> Self {
        let guard = TempDir::new().expect("temp dir");
        let log = AuditLog::new(guard.path());
        Self { _guard: guard, log }
    }

    /// Appends one line with the shape most calls have, overriding the few
    /// fields a given test cares about.
    fn append(&self, session: &str, tool: &str, args: &Value) {
        self.log.append(&AuditRecord {
            session_id: session,
            agent_id: DEFAULT_AGENT_ID,
            turn_id: "turn-1",
            call_id: "call-1",
            tool,
            skill: "",
            handoff: "",
            routine: "",
            decision: AuditDecision::Auto,
            policy_reason: "an ordinary read inside the workspace",
            args,
            outcome: Outcome::Ok,
            duration_ms: 7,
            bytes_in: 0,
            bytes_out: 12,
            error_code: None,
            artifact: None,
        });
    }

    /// The raw lines of the file, oldest first.
    fn lines(&self) -> Vec<Value> {
        let text = fs::read_to_string(self.log.path()).unwrap_or_default();
        text.lines()
            .map(|line| serde_json::from_str(line).expect("every line is JSON"))
            .collect()
    }

    /// Appends a raw string to the file, as a crash or another writer would.
    fn append_raw(&self, raw: &str) {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log.path())
            .expect("open the log");
        file.write_all(raw.as_bytes()).expect("write");
    }
}

#[test]
fn a_line_carries_the_documented_field_names() {
    // `src/ipc/bindings.ts` mirrors these by hand until Phase 5 generates it.
    // Renaming a field in Rust without renaming it there fails here rather
    // than showing up as `undefined` in the audit drawer.
    let fixture = Fixture::new();
    fixture.append("s1", "fs_read", &json!({ "path": "a.txt" }));

    let line = &fixture.lines()[0];
    for field in [
        "ts",
        "session_id",
        "turn_id",
        "call_id",
        "tool",
        "decision",
        "policy_reason",
        "args_digest",
        "args_redacted",
        "outcome",
        "duration_ms",
        "bytes_in",
        "bytes_out",
        "error_code",
    ] {
        assert!(
            line.get(field).is_some(),
            "`{field}` is missing from an audit line: {line}"
        );
    }

    assert_eq!(line["decision"], "auto");
    assert_eq!(line["outcome"], "ok");
    assert_eq!(line["error_code"], Value::Null);
    assert!(
        line["ts"].as_str().expect("a timestamp").ends_with('Z'),
        "timestamps are UTC so the file sorts by them"
    );
}

#[test]
fn the_decision_vocabulary_is_the_one_the_ui_branches_on() {
    let fixture = Fixture::new();
    let args = json!({ "path": "a.txt" });

    for (decision, expected) in [
        (AuditDecision::Auto, "auto"),
        (AuditDecision::AllowOnce, "allow_once"),
        (AuditDecision::AllowSession, "allow_session"),
        (AuditDecision::Deny, "deny"),
    ] {
        fixture.log.append(&AuditRecord {
            session_id: "s1",
            agent_id: DEFAULT_AGENT_ID,
            turn_id: "t1",
            call_id: "c1",
            tool: "fs_write",
            skill: "",
            handoff: "",
            routine: "",
            decision,
            policy_reason: "because",
            args: &args,
            outcome: Outcome::Denied,
            duration_ms: 0,
            bytes_in: 0,
            bytes_out: 0,
            error_code: Some(ErrorCode::Denied),
            artifact: None,
        });

        let line = fixture.lines().pop().expect("a line");
        assert_eq!(line["decision"], expected);
        assert_eq!(line["error_code"], "E_DENIED");
    }
}

#[test]
fn every_outcome_has_a_stable_spelling() {
    let fixture = Fixture::new();
    let args = json!({ "path": "a.txt" });

    for (outcome, expected) in [
        (Outcome::Ok, "ok"),
        (Outcome::Error, "error"),
        (Outcome::Denied, "denied"),
        (Outcome::Cancelled, "cancelled"),
    ] {
        fixture.log.append(&AuditRecord {
            session_id: "s1",
            agent_id: DEFAULT_AGENT_ID,
            turn_id: "t1",
            call_id: "c1",
            tool: "fs_read",
            skill: "",
            handoff: "",
            routine: "",
            decision: AuditDecision::Auto,
            policy_reason: "because",
            args: &args,
            outcome,
            duration_ms: 0,
            bytes_in: 0,
            bytes_out: 0,
            error_code: None,
            artifact: None,
        });

        assert_eq!(fixture.lines().pop().expect("a line")["outcome"], expected);
    }
}

#[test]
fn appends_keep_their_order_and_do_not_rewrite_earlier_lines() {
    let fixture = Fixture::new();
    let args = json!({ "path": "a.txt" });

    for tool in ["fs_list", "fs_read", "fs_write"] {
        fixture.append("s1", tool, &args);
    }

    let lines = fixture.lines();
    let tools: Vec<&str> = lines
        .iter()
        .map(|line| line["tool"].as_str().expect("a tool name"))
        .collect();
    assert_eq!(tools, vec!["fs_list", "fs_read", "fs_write"]);
}

#[test]
fn the_contents_of_a_written_file_never_reach_the_log() {
    let fixture = Fixture::new();
    let secret = "AKIAIOSFODNN7EXAMPLE and a private key besides";
    fixture.append(
        "s1",
        "fs_write",
        &json!({ "path": ".env", "content": secret }),
    );

    let raw = fs::read_to_string(fixture.log.path()).expect("the log exists");

    assert!(
        !raw.contains("AKIA"),
        "a log that quoted file contents would be the worst file on the machine"
    );
    assert!(raw.contains(".env"), "the path is exactly what is wanted");
    assert!(raw.contains("bytes"), "the size stands in for the content");
}

#[test]
fn identical_calls_digest_identically_and_different_ones_do_not() {
    let fixture = Fixture::new();
    fixture.append("s1", "fs_read", &json!({ "path": "a.txt", "limit": 10 }));
    fixture.append("s1", "fs_read", &json!({ "limit": 10, "path": "a.txt" }));
    fixture.append("s1", "fs_read", &json!({ "path": "b.txt", "limit": 10 }));

    let lines = fixture.lines();
    let digests: Vec<&str> = lines
        .iter()
        .map(|line| line["args_digest"].as_str().expect("a digest"))
        .collect();

    assert_eq!(digests[0], digests[1], "key order is not part of the call");
    assert_ne!(
        digests[0], digests[2],
        "a different path is a different call"
    );
    assert_eq!(digests[0].len(), 64);
}

#[test]
fn tailing_returns_the_newest_first() {
    let fixture = Fixture::new();
    let args = json!({ "path": "a.txt" });
    for tool in ["fs_list", "fs_read", "fs_write"] {
        fixture.append("s1", tool, &args);
    }

    let entries = fixture.log.tail(10, None).expect("tail");

    let tools: Vec<&str> = entries.iter().map(|entry| entry.tool.as_str()).collect();
    assert_eq!(tools, vec!["fs_write", "fs_read", "fs_list"]);
}

#[test]
fn a_limit_counts_entries_the_caller_will_see() {
    let fixture = Fixture::new();
    let args = json!({ "path": "a.txt" });
    fixture.append("s1", "fs_read", &args);
    for _ in 0..5 {
        fixture.append("s2", "fs_read", &args);
    }
    fixture.append("s1", "fs_write", &args);

    // The two `s1` lines are the first and the last of eight; a limit applied
    // before the filter would return one of them, or none.
    let entries = fixture.log.tail(2, Some("s1")).expect("tail");

    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|entry| entry.session_id == "s1"));
    assert_eq!(entries[0].tool, "fs_write");
}

#[test]
fn a_limit_of_zero_returns_nothing_rather_than_everything() {
    let fixture = Fixture::new();
    fixture.append("s1", "fs_read", &json!({ "path": "a.txt" }));

    assert!(fixture.log.tail(0, None).expect("tail").is_empty());
}

#[test]
fn filtering_by_a_session_that_wrote_nothing_is_empty_not_an_error() {
    let fixture = Fixture::new();
    fixture.append("s1", "fs_read", &json!({ "path": "a.txt" }));

    assert!(fixture.log.tail(10, Some("s9")).expect("tail").is_empty());
}

#[test]
fn a_line_a_killed_process_left_half_written_is_skipped() {
    let fixture = Fixture::new();
    let args = json!({ "path": "a.txt" });
    fixture.append("s1", "fs_list", &args);
    fixture.append_raw("{\"ts\":\"2026-08-29T09:00:00.000Z\",\"session");

    // The good line is still there, and a later append starts on its own line
    // only if the fragment ended with a newline — it did not, so the writer's
    // own newline terminates the fragment instead. Either way the reader must
    // not lose the entries around it.
    let entries = fixture.log.tail(10, None).expect("tail");

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].tool, "fs_list");
}

#[test]
fn a_line_with_an_unknown_shape_does_not_stop_the_read() {
    let fixture = Fixture::new();
    let args = json!({ "path": "a.txt" });
    fixture.append("s1", "fs_list", &args);
    fixture.append_raw("{\"hello\":\"from another tool entirely\"}\n");
    fixture.append("s1", "fs_read", &args);

    let entries = fixture.log.tail(10, None).expect("tail");

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].tool, "fs_read");
}

#[test]
fn blank_lines_are_not_entries() {
    let fixture = Fixture::new();
    fixture.append("s1", "fs_list", &json!({ "path": "a.txt" }));
    fixture.append_raw("\n\n");

    assert_eq!(fixture.log.tail(10, None).expect("tail").len(), 1);
}

#[test]
fn the_log_is_not_created_until_something_is_audited() {
    let fixture = Fixture::new();

    assert!(
        !fixture.log.path().exists(),
        "a run with no tool call leaves no file behind"
    );

    fixture.append("s1", "fs_list", &json!({ "path": "." }));
    assert!(fixture.log.path().is_file());
}

#[test]
fn a_log_in_a_directory_that_does_not_exist_yet_is_created_with_it() {
    let guard = TempDir::new().expect("temp dir");
    let log = AuditLog::new(&guard.path().join("nested").join("deeper"));

    log.append(&AuditRecord {
        session_id: "s1",
        agent_id: DEFAULT_AGENT_ID,
        turn_id: "t1",
        call_id: "c1",
        tool: "fs_list",
        skill: "",
        handoff: "",
        routine: "",
        decision: AuditDecision::Auto,
        policy_reason: "because",
        args: &json!({ "path": "." }),
        outcome: Outcome::Ok,
        duration_ms: 0,
        bytes_in: 0,
        bytes_out: 0,
        error_code: None,
        artifact: None,
    });

    assert_eq!(log.tail(10, None).expect("tail").len(), 1);
}

#[test]
fn a_long_log_is_read_from_its_end() {
    let fixture = Fixture::new();
    let args = json!({ "path": "a.txt" });

    // One real line, then many copies of it written in a single pass: the
    // point of the test is a file far past the 4 MB read window, and
    // twenty thousand separate appends would only be measuring how fast this
    // machine opens a file.
    fixture.append("s1", "fs_read", &args);
    let one = fs::read_to_string(fixture.log.path()).expect("the log exists");
    fixture.append_raw(&one.repeat(20_000));

    fixture.append("s1", "fs_write", &args);

    assert!(
        fs::metadata(fixture.log.path())
            .expect("the log exists")
            .len()
            > 4 * 1024 * 1024,
        "the file has to be longer than the window for this to prove anything"
    );

    let entries = fixture.log.tail(5, None).expect("tail");

    assert_eq!(entries.len(), 5);
    assert_eq!(
        entries[0].tool, "fs_write",
        "the newest line is found however long the file is"
    );
}
