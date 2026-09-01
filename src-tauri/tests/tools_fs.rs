//! The filesystem tools, driven the way the turn loop will drive them.
//!
//! Nothing here calls a tool directly. Every test goes
//! `policy::decide` → `tools::run`, because that pipeline *is* what Phase 4
//! delivers: a tool that could be reached without a decision would be a tool
//! outside the gate, and a test that skipped the decision would not notice.
//!
//! The exit criterion of the phase is at the bottom: one test drives
//! `fs_list`, `fs_read` and `fs_write` through policy and then finds the
//! expected lines on disk in the audit log.

use std::fs;
use std::path::{Path, PathBuf};

use aegis_lib::audit::{AuditDecision, AuditLog, Outcome};
use aegis_lib::policy::{decide, tool, Decision, GrantStore, PolicyCtx};
use aegis_lib::tools::{self, NullProgress, ToolCtx, ToolOutcome, READ_MAX_BYTES};
use aegis_lib::HandoffCtx;
use aegis_lib::{MemoryStore, SkillCtx, DEFAULT_AGENT_ID};
use serde_json::{json, Value};
use tempfile::TempDir;

/// A workspace, a directory outside it, a grant store and a fresh audit log.
struct Fixture {
    _workspace_guard: TempDir,
    _outside_guard: TempDir,
    _data_guard: TempDir,
    workspace: PathBuf,
    outside: PathBuf,
    grants: GrantStore,
    audit: AuditLog,
    /// Where a capture would go. Nothing in this file captures anything; the
    /// path is here because a tool call is not runnable without one.
    captures: PathBuf,
    /// An empty memory store. Nothing here remembers anything; the store is
    /// only there because a tool call is not runnable without one.
    memories: MemoryStore,
    /// `tools::run` is `async` for the sake of one tool, `shell_exec`. The
    /// filesystem tools have nothing to await, so rather than turn thirty
    /// tests into async ones this drives the future to completion here — the
    /// tests stay a description of the pipeline instead of a description of
    /// the runtime.
    runtime: tokio::runtime::Runtime,
}

impl Fixture {
    fn new() -> Self {
        let workspace_guard = TempDir::new().expect("temp dir");
        let outside_guard = TempDir::new().expect("temp dir");
        let data_guard = TempDir::new().expect("temp dir");
        let audit = AuditLog::new(data_guard.path());
        let captures = data_guard.path().join("captures");
        let memories = MemoryStore::load(data_guard.path());

        Self {
            workspace: dunce::canonicalize(workspace_guard.path()).expect("canonical"),
            outside: dunce::canonicalize(outside_guard.path()).expect("canonical"),
            _workspace_guard: workspace_guard,
            _outside_guard: outside_guard,
            _data_guard: data_guard,
            grants: GrantStore::new(),
            audit,
            captures,
            memories,
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a tokio runtime"),
        }
    }

    /// Creates a file in the workspace, parents included.
    fn file(&self, relative: &str, content: &str) -> PathBuf {
        let path = self.workspace.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(&path, content).expect("write");
        path
    }

    /// Creates a directory in the workspace.
    fn dir(&self, relative: &str) -> PathBuf {
        let path = self.workspace.join(relative);
        fs::create_dir_all(&path).expect("mkdir");
        path
    }

    /// Runs one call end to end: policy decides, and whatever it decided is
    /// carried out and audited.
    ///
    /// An ask is treated as the user having answered `allow_once`, which is
    /// what Phase 6 will do with the same decision; a hard denial is refused
    /// without running anything. This is the whole of the turn loop's tool
    /// step, minus the events.
    fn call(&self, tool_name: &str, args: Value) -> ToolOutcome {
        let ctx = PolicyCtx::new("session-1", Some(&self.workspace), &self.grants);
        let cancel = tokio_util::sync::CancellationToken::new();
        let tools_ctx = ToolCtx {
            session_id: "session-1",
            agent_id: DEFAULT_AGENT_ID,
            turn_id: "turn-1",
            call_id: "call-1",
            audit: &self.audit,
            captures: &self.captures,
            args: &args,
            progress: &NullProgress,
            cancel: &cancel,
            // Nothing here runs a skill; the runner has its own tests.
            skills: SkillCtx {
                library: &self.captures,
                workspace: None,
                tools: &[],
                active: None,
            },
            // Nothing here remembers anything either.
            memories: &self.memories,
            connectors: aegis_lib::Connectors::none(),
            handoffs: HandoffCtx {
                bus: None,
                open: None,
            },
            routine: "",
        };

        match decide(&ctx, tool_name, args.clone()) {
            Decision::Auto { call, reason } => {
                self.runtime
                    .block_on(tools::run(&tools_ctx, AuditDecision::Auto, reason, &call))
            }
            Decision::Ask { call, request } => self.runtime.block_on(tools::run(
                &tools_ctx,
                AuditDecision::AllowOnce,
                &request.reason,
                &call,
            )),
            Decision::Deny { code, reason } => {
                tools::refuse(&tools_ctx, tool_name, AuditDecision::Deny, code, &reason)
            }
        }
    }

    /// Every audit line written so far, oldest first.
    fn audit_lines(&self) -> Vec<Value> {
        let text = fs::read_to_string(self.audit.path()).unwrap_or_default();
        text.lines()
            .map(|line| serde_json::from_str(line).expect("every line is JSON"))
            .collect()
    }
}

/// The envelope as the model would receive it: parsed back from the JSON
/// string that goes into a `tool` message, so a field that fails to serialize
/// fails a test rather than reaching a model.
fn envelope(outcome: &ToolOutcome) -> Value {
    serde_json::from_str(&outcome.result.to_json()).expect("the envelope is valid JSON")
}

// ---------------------------------------------------------------------------
// fs_list
// ---------------------------------------------------------------------------

#[test]
fn listing_a_workspace_directory_needs_no_approval() {
    let fixture = Fixture::new();
    fixture.file("README.md", "hello");
    fixture.dir("src");

    let outcome = fixture.call(tool::FS_LIST, json!({ "path": "." }));
    let envelope = envelope(&outcome);

    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["tool"], "fs_list");
    // Directories first, then files with their sizes.
    assert_eq!(outcome.result.content, "src/\nREADME.md\t5");
    assert_eq!(envelope["meta"]["entries"], 2);
    assert_eq!(outcome.audit.decision, AuditDecision::Auto);
}

#[test]
fn a_listing_says_when_it_is_showing_part_of_a_directory() {
    let fixture = Fixture::new();
    for index in 0..5 {
        fixture.file(&format!("f{index}.txt"), "x");
    }

    let outcome = fixture.call(tool::FS_LIST, json!({ "path": ".", "max_entries": 2 }));
    let envelope = envelope(&outcome);

    assert_eq!(envelope["truncated"], true);
    assert_eq!(envelope["meta"]["entries"], 2);
    assert_eq!(envelope["meta"]["total_entries"], 5);
    assert!(
        outcome.result.content.contains("3 more entries not shown"),
        "the content says so too: {}",
        outcome.result.content
    );
}

#[test]
fn listing_a_directory_that_is_not_there_is_a_readable_failure() {
    let fixture = Fixture::new();

    let outcome = fixture.call(tool::FS_LIST, json!({ "path": "nowhere" }));
    let envelope = envelope(&outcome);

    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "E_TOOL_FAILED");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .expect("a message")
            .contains("does not exist"),
        "{envelope}"
    );
    assert_eq!(outcome.audit.outcome, Outcome::Error);
}

// ---------------------------------------------------------------------------
// fs_read
// ---------------------------------------------------------------------------

#[test]
fn reading_a_small_workspace_file_needs_no_approval() {
    let fixture = Fixture::new();
    fixture.file("src/main.rs", "fn main() {}");

    let outcome = fixture.call(tool::FS_READ, json!({ "path": "src/main.rs" }));
    let envelope = envelope(&outcome);

    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["content"], "fn main() {}");
    assert_eq!(envelope["truncated"], false);
    assert_eq!(envelope["bytes"], 12);
    assert_eq!(envelope["meta"]["bytes_total"], 12);
    assert_eq!(outcome.audit.decision, AuditDecision::Auto);
}

#[test]
fn a_read_stops_at_the_cap_and_says_so() {
    let fixture = Fixture::new();
    let size = (READ_MAX_BYTES + 4096) as usize;
    fixture.file("big.txt", &"a".repeat(size));

    // Over 1 MB would be an ask; this file is large enough to pass the cap and
    // small enough that policy still auto-allows it.
    let outcome = fixture.call(tool::FS_READ, json!({ "path": "big.txt" }));
    let envelope = envelope(&outcome);

    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["truncated"], true);
    assert_eq!(
        outcome.result.content.len() as u64,
        READ_MAX_BYTES,
        "the cap is the tool's, not the caller's"
    );
    assert_eq!(envelope["bytes"], size, "the whole file's size is reported");
}

#[test]
fn a_read_window_can_be_moved_with_an_offset() {
    let fixture = Fixture::new();
    fixture.file("a.txt", "0123456789");

    let outcome = fixture.call(
        tool::FS_READ,
        json!({ "path": "a.txt", "offset": 4, "limit": 3 }),
    );
    let envelope = envelope(&outcome);

    assert_eq!(envelope["content"], "456");
    assert_eq!(
        envelope["truncated"], true,
        "there is more after the window"
    );
    assert_eq!(envelope["meta"]["offset"], 4);
}

#[test]
fn a_file_that_is_not_text_is_reported_rather_than_mangled() {
    let fixture = Fixture::new();
    fs::write(fixture.workspace.join("blob.bin"), [0xFF, 0xFE, 0x00, 0x41]).expect("write");

    let outcome = fixture.call(tool::FS_READ, json!({ "path": "blob.bin" }));
    let envelope = envelope(&outcome);

    assert_eq!(envelope["ok"], false);
    assert!(
        envelope["error"]["message"]
            .as_str()
            .expect("a message")
            .contains("not UTF-8"),
        "{envelope}"
    );
}

#[test]
fn reading_a_directory_points_at_the_right_tool() {
    let fixture = Fixture::new();
    fixture.dir("src");

    let outcome = fixture.call(tool::FS_READ, json!({ "path": "src" }));
    let envelope = envelope(&outcome);

    assert_eq!(envelope["ok"], false);
    assert!(
        envelope["error"]["message"]
            .as_str()
            .expect("a message")
            .contains("fs_list"),
        "{envelope}"
    );
}

#[test]
fn a_read_that_leaves_the_workspace_is_asked_about_and_then_runs() {
    let fixture = Fixture::new();
    let secret = fixture.outside.join("notes.txt");
    fs::write(&secret, "outside").expect("write");

    let outcome = fixture.call(
        tool::FS_READ,
        json!({ "path": secret.to_string_lossy().into_owned() }),
    );

    assert_eq!(envelope(&outcome)["content"], "outside");
    assert_eq!(
        outcome.audit.decision,
        AuditDecision::AllowOnce,
        "outside the workspace is never auto-allowed"
    );
    assert!(
        outcome
            .audit
            .policy_reason
            .contains("outside the workspace"),
        "{}",
        outcome.audit.policy_reason
    );
}

// ---------------------------------------------------------------------------
// fs_write
// ---------------------------------------------------------------------------

#[test]
fn a_write_lands_on_disk_exactly_as_asked() {
    let fixture = Fixture::new();

    let outcome = fixture.call(
        tool::FS_WRITE,
        json!({ "path": "notes.md", "content": "# Notes\n" }),
    );
    let envelope = envelope(&outcome);

    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["meta"]["created"], true);
    assert_eq!(
        fs::read_to_string(fixture.workspace.join("notes.md")).expect("the file exists"),
        "# Notes\n",
        "nothing is added to what the model wrote"
    );
    assert_eq!(outcome.audit.bytes_in, 8);
}

#[test]
fn a_write_replaces_what_was_there() {
    let fixture = Fixture::new();
    fixture.file("notes.md", "old, and longer than the new content");

    let outcome = fixture.call(
        tool::FS_WRITE,
        json!({ "path": "notes.md", "content": "new" }),
    );

    assert_eq!(envelope(&outcome)["meta"]["created"], false);
    assert_eq!(
        fs::read_to_string(fixture.workspace.join("notes.md")).expect("the file exists"),
        "new",
        "a replace truncates rather than overwriting in place"
    );
}

#[test]
fn a_write_creates_parents_only_when_it_was_told_to() {
    let fixture = Fixture::new();

    let refused = fixture.call(
        tool::FS_WRITE,
        json!({ "path": "a/b/c.txt", "content": "x" }),
    );
    assert_eq!(envelope(&refused)["ok"], false);
    assert_eq!(
        envelope(&refused)["error"]["code"],
        "E_PATH_INVALID",
        "policy refuses it before the tool is reached"
    );

    let allowed = fixture.call(
        tool::FS_WRITE,
        json!({ "path": "a/b/c.txt", "content": "x", "create_dirs": true }),
    );
    assert_eq!(envelope(&allowed)["ok"], true);
    assert!(fixture.workspace.join("a/b/c.txt").is_file());
}

#[test]
fn a_write_onto_a_directory_never_reaches_the_tool() {
    let fixture = Fixture::new();
    fixture.dir("src");

    let outcome = fixture.call(tool::FS_WRITE, json!({ "path": "src", "content": "x" }));
    let envelope = envelope(&outcome);

    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "E_PATH_INVALID");
    assert_eq!(outcome.audit.outcome, Outcome::Denied);
    assert!(
        fixture.workspace.join("src").is_dir(),
        "the directory is untouched"
    );
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

#[test]
fn a_refusal_is_an_envelope_the_model_can_read() {
    let fixture = Fixture::new();

    // An argument that names no path cannot be approved into meaning one, so
    // policy refuses it outright rather than offering a dialog (PLAN 3.2).
    let outcome = fixture.call(tool::FS_READ, json!({ "path": "   " }));
    let envelope = envelope(&outcome);

    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["content"], "");
    assert_eq!(envelope["error"]["code"], "E_PATH_INVALID");
    assert_eq!(outcome.audit.outcome, Outcome::Denied);
    assert_eq!(outcome.audit.decision, AuditDecision::Deny);
}

#[test]
fn a_tool_this_build_does_not_have_says_so_rather_than_failing_silently() {
    let fixture = Fixture::new();

    // A model that invents a tool must get an answer it can act on, and the
    // refusal has to happen in policy — before anything is resolved, let alone
    // run.
    let outcome = fixture.call("fs_delete", json!({ "path": "a.txt" }));
    let envelope = envelope(&outcome);

    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "E_TOOL_FAILED");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .expect("a message")
            .contains("fs_delete"),
        "{envelope}"
    );
    assert_eq!(outcome.audit.outcome, Outcome::Denied);
}

#[test]
fn the_tools_offered_to_the_model_are_the_ones_this_build_runs() {
    let names: Vec<String> = tools::schemas()
        .iter()
        .map(|schema| schema["function"]["name"].as_str().unwrap_or("").to_owned())
        .collect();

    assert_eq!(
        names,
        vec![
            "fs_list",
            "fs_read",
            "fs_write",
            "shell_exec",
            "screen_capture",
            // Phase 13. Loading a runbook and recording what came of it are
            // verbs like any other, so they are registry entries like any
            // other — which is what puts a skill run through the same policy,
            // the same envelope and the same audit line as everything else.
            "skill_run",
            "skill_return",
            // Phase 14, on the same terms. Remembering something is a verb,
            // and one of the two is mutating — so `memory_write` goes through
            // the approval dialog exactly as `fs_write` does, and the dialog
            // shows the sentence that would be remembered.
            "memory_write",
            "memory_search",
            "handoff_delegate",
            "handoff_return",
        ]
    );
}

/// The exit criterion of Phase 4 (PLAN § 6).
///
/// One session drives all three filesystem tools through policy, and the
/// expected lines are found on disk afterwards — in order, one per call, with
/// the decision and the outcome each recorded.
#[test]
fn the_three_fs_tools_run_through_policy_and_land_in_the_audit_log() {
    let fixture = Fixture::new();
    fixture.file("src/main.rs", "fn main() {}");

    let listed = fixture.call(tool::FS_LIST, json!({ "path": "src" }));
    let read = fixture.call(tool::FS_READ, json!({ "path": "src/main.rs" }));
    let written = fixture.call(
        tool::FS_WRITE,
        json!({ "path": "src/main.rs", "content": "fn main() { println!(\"hi\"); }" }),
    );

    assert!(listed.result.ok && read.result.ok && written.result.ok);
    assert_eq!(
        fs::read_to_string(fixture.workspace.join("src/main.rs")).expect("the file exists"),
        "fn main() { println!(\"hi\"); }"
    );

    let lines = fixture.audit_lines();
    assert_eq!(lines.len(), 3, "exactly one line per call");

    let tools_logged: Vec<&str> = lines
        .iter()
        .map(|line| line["tool"].as_str().expect("a tool name"))
        .collect();
    assert_eq!(tools_logged, vec!["fs_list", "fs_read", "fs_write"]);

    for line in &lines {
        assert_eq!(line["session_id"], "session-1");
        assert_eq!(line["turn_id"], "turn-1");
        assert_eq!(line["outcome"], "ok");
        assert_eq!(line["error_code"], Value::Null);
        assert!(
            line["args_digest"]
                .as_str()
                .expect("a digest")
                .chars()
                .all(|c| c.is_ascii_hexdigit()),
            "the digest is hex"
        );
    }

    // Reads are automatic inside the workspace; writes always ask (PLAN 3).
    assert_eq!(lines[0]["decision"], "auto");
    assert_eq!(lines[1]["decision"], "auto");
    assert_eq!(lines[2]["decision"], "allow_once");

    // The write's content is described, never quoted.
    let redacted = lines[2]["args_redacted"].as_str().expect("a redacted copy");
    assert!(redacted.contains("src/main.rs"), "{redacted}");
    assert!(!redacted.contains("println"), "{redacted}");
    assert_eq!(lines[2]["bytes_in"], 29);
}

/// A hard denial is audited exactly like an execution: same file, same shape.
///
/// This is what makes the log answerable to "did the agent try?" and not only
/// "what did the agent do".
#[test]
fn a_call_that_never_ran_is_still_in_the_log() {
    let fixture = Fixture::new();
    fixture.dir("src");

    fixture.call(tool::FS_WRITE, json!({ "path": "src", "content": "x" }));

    let lines = fixture.audit_lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["tool"], "fs_write");
    assert_eq!(lines[0]["decision"], "deny");
    assert_eq!(lines[0]["outcome"], "denied");
    assert_eq!(lines[0]["error_code"], "E_PATH_INVALID");
    assert_eq!(lines[0]["duration_ms"], 0);
}

/// A session grant collapses the prompt, and the log says which calls were
/// covered by it rather than by policy.
#[test]
fn a_session_grant_shows_up_in_the_log_as_the_reason_a_write_ran() {
    let fixture = Fixture::new();
    fixture
        .grants
        .insert("session-1", aegis_lib::Grant::FsWrite);

    let outcome = fixture.call(
        tool::FS_WRITE,
        json!({ "path": "a.txt", "content": "granted" }),
    );

    assert!(outcome.result.ok);
    assert_eq!(
        outcome.audit.decision,
        AuditDecision::Auto,
        "a grant means no prompt was raised"
    );
    assert!(
        outcome
            .audit
            .policy_reason
            .contains("allowed for this session"),
        "{}",
        outcome.audit.policy_reason
    );
}

/// Policy resolves a path once; the tool operates on what policy resolved.
///
/// The check is indirect on purpose — there is no way to hand a tool a
/// different path than the one that was judged, which is the property being
/// asserted. What is observable is that the file that changed is the resolved
/// one.
#[test]
fn a_tool_touches_the_path_policy_resolved_and_no_other() {
    let fixture = Fixture::new();
    fixture.dir("src/deep");
    fixture.file("target.txt", "original");

    let outcome = fixture.call(
        tool::FS_WRITE,
        json!({ "path": "src/deep/../../target.txt", "content": "resolved" }),
    );

    assert!(outcome.result.ok);
    assert_eq!(
        fs::read_to_string(fixture.workspace.join("target.txt")).expect("the file exists"),
        "resolved"
    );
    let touched = envelope(&outcome)["meta"]["path"]
        .as_str()
        .expect("a path")
        .to_owned();
    assert!(
        !touched.contains(".."),
        "the envelope names the resolved path: {touched}"
    );
    assert!(Path::new(&touched).starts_with(&fixture.workspace));
}
