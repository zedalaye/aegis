//! The shell tool, driven the way the turn loop drives it.
//!
//! Like `tools_fs.rs`, nothing here calls the tool directly: every test goes
//! `policy::decide` → `tools::run`, because a command that could be reached
//! without a decision would be a command outside the gate.
//!
//! The programs under test are written into the workspace by the fixture
//! rather than borrowed from the platform. That is not an affectation — it is
//! the only way to ask for *exactly* eighty kilobytes of output, or an exit
//! code of 3, or a process that will still be running in thirty seconds, on
//! three operating systems that agree on none of their built-in commands. It
//! also means the Windows runs exercise the `.cmd` launch path (PLAN 5.1) for
//! real, which is the platform detail most likely to break.
//!
//! The exit criterion of the phase is at the bottom: a command runs under
//! approval, its output streams while it runs, and the audit log has the line.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use aegis_lib::audit::{AuditDecision, AuditLog, Outcome};
use aegis_lib::policy::{decide, tool, Decision, Grant, GrantStore, PolicyCtx};
use aegis_lib::tools::{self, ProgressSink, Stream, ToolCtx, ToolOutcome};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

/// A [`ProgressSink`] that keeps everything it is handed.
#[derive(Debug, Default)]
struct Recorder {
    chunks: Mutex<Vec<(Stream, String)>>,
}

impl Recorder {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<(Stream, String)>> {
        self.chunks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Everything that arrived, in arrival order.
    fn text(&self) -> String {
        self.lock().iter().map(|(_, text)| text.as_str()).collect()
    }

    /// Everything that arrived on one pipe.
    fn text_of(&self, wanted: Stream) -> String {
        self.lock()
            .iter()
            .filter(|(stream, _)| *stream == wanted)
            .map(|(_, text)| text.as_str())
            .collect()
    }

    fn frames(&self) -> usize {
        self.lock().len()
    }
}

impl ProgressSink for Recorder {
    fn chunk(&self, stream: Stream, text: &str) {
        self.lock().push((stream, text.to_owned()));
    }
}

/// A workspace with programs in it, a directory outside, and a fresh log.
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
}

impl Fixture {
    fn new() -> Self {
        let workspace_guard = TempDir::new().expect("temp dir");
        let outside_guard = TempDir::new().expect("temp dir");
        let data_guard = TempDir::new().expect("temp dir");
        let audit = AuditLog::new(data_guard.path());
        let captures = data_guard.path().join("captures");

        Self {
            workspace: dunce::canonicalize(workspace_guard.path()).expect("canonical"),
            outside: dunce::canonicalize(outside_guard.path()).expect("canonical"),
            _workspace_guard: workspace_guard,
            _outside_guard: outside_guard,
            _data_guard: data_guard,
            grants: GrantStore::new(),
            audit,
            captures,
        }
    }

    /// Writes a runnable program into the workspace and returns the `program`
    /// argument that names it.
    ///
    /// The two bodies are the same program written for the two shells that
    /// will interpret them. The name is returned with a `./` on it so
    /// resolution treats it as a path rather than a PATH lookup — the point
    /// being to run *this* file and not something with the same name that
    /// happens to be installed on the machine running the tests.
    fn script(&self, name: &str, windows: &str, unix: &str) -> String {
        if cfg!(windows) {
            let path = self.workspace.join(format!("{name}.cmd"));
            fs::write(&path, format!("@echo off\r\n{windows}\r\n")).expect("write");
            format!("./{name}.cmd")
        } else {
            let path = self.workspace.join(name);
            fs::write(&path, format!("#!/bin/sh\n{unix}\n")).expect("write");
            make_executable(&path);
            format!("./{name}")
        }
    }

    /// What policy says about a call, without running it.
    fn judge(&self, args: &Value) -> Decision {
        let ctx = PolicyCtx::new("session-1", Some(&self.workspace), &self.grants);
        decide(&ctx, tool::SHELL_EXEC, args.clone())
    }

    /// One call end to end, as the turn loop would: policy decides, an ask is
    /// answered `allow_once`, and whatever is left is executed and audited.
    async fn call(&self, args: Value) -> ToolOutcome {
        self.watched(args, &Recorder::default(), &CancellationToken::new())
            .await
    }

    /// [`Fixture::call`], with somewhere for the output to go and something to
    /// stop it with.
    async fn watched(
        &self,
        args: Value,
        progress: &dyn ProgressSink,
        cancel: &CancellationToken,
    ) -> ToolOutcome {
        let ctx = ToolCtx {
            session_id: "session-1",
            turn_id: "turn-1",
            call_id: "call-1",
            audit: &self.audit,
            captures: &self.captures,
            args: &args,
            progress,
            cancel,
        };

        match self.judge(&args) {
            Decision::Auto { call, reason } => {
                tools::run(&ctx, AuditDecision::Auto, reason, &call).await
            }
            Decision::Ask { call, request } => {
                tools::run(&ctx, AuditDecision::AllowOnce, &request.reason, &call).await
            }
            Decision::Deny { code, reason } => {
                tools::refuse(&ctx, tool::SHELL_EXEC, AuditDecision::Deny, code, &reason)
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

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}

/// The envelope as the model would receive it.
fn envelope(outcome: &ToolOutcome) -> Value {
    serde_json::from_str(&outcome.result.to_json()).expect("the envelope is valid JSON")
}

/// A line long enough that a few hundred of them pass the 64 KB cap.
const LONG_LINE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\
                         0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\
                         0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

/// PLAN 3: every `shell_exec` row is an ask. There is no auto-allow for
/// running code, wherever the working directory is.
#[test]
fn running_a_command_always_asks() {
    let fixture = Fixture::new();
    let program = fixture.script("hello", "echo hello world", "echo hello world");

    match fixture.judge(&json!({ "program": program })) {
        Decision::Ask { request, .. } => {
            assert_eq!(request.title, "Run shell command");
            assert!(request.summary.contains("hello"), "{}", request.summary);
            assert!(
                request.grant.is_some(),
                "a command inside the workspace offers a session grant"
            );
        }
        other => panic!("expected an ask, got {other:?}"),
    }
}

/// PLAN 3.1: the grant is keyed on the program, not on the tool. Approving one
/// command for the session must not approve every other command.
#[tokio::test]
async fn a_session_grant_covers_one_program_and_no_other() {
    let fixture = Fixture::new();
    let allowed = fixture.script("allowed", "echo yes", "echo yes");
    let other = fixture.script("other", "echo no", "echo no");

    assert!(fixture.grants.insert("session-1", Grant::shell(&allowed)));

    match fixture.judge(&json!({ "program": allowed })) {
        Decision::Auto { .. } => {}
        other => panic!("the granted program should not ask again, got {other:?}"),
    }
    match fixture.judge(&json!({ "program": other })) {
        Decision::Ask { .. } => {}
        other => panic!("a different program still asks, got {other:?}"),
    }
}

/// PLAN 3: outside the workspace it asks every time, and no grant is offered —
/// "the rest of this session" is not a scope anyone can picture for the whole
/// filesystem.
#[test]
fn a_working_directory_outside_the_workspace_offers_no_grant() {
    let fixture = Fixture::new();
    let program = fixture.script("hello", "echo hello", "echo hello");

    let args = json!({
        "program": program,
        "cwd": fixture.outside.display().to_string(),
    });

    match fixture.judge(&args) {
        Decision::Ask { request, .. } => {
            assert!(request.grant.is_none(), "no grant outside the workspace");
            assert!(request.reason.contains("outside"), "{}", request.reason);
        }
        other => panic!("expected an ask, got {other:?}"),
    }
}

/// PLAN 3.2: a program that resolves to this application is refused outright,
/// because approving it could not mean anything sane.
#[test]
fn aegis_will_not_run_itself() {
    let fixture = Fixture::new();
    let exe = PathBuf::from(if cfg!(windows) { "aegis.exe" } else { "aegis" });

    let ctx = PolicyCtx::new("session-1", Some(&fixture.workspace), &fixture.grants)
        .with_self_exe(Some(&exe));

    match decide(&ctx, tool::SHELL_EXEC, json!({ "program": "aegis" })) {
        Decision::Deny { reason, .. } => assert!(reason.contains("itself"), "{reason}"),
        other => panic!("expected a denial, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Running
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_command_runs_and_its_output_comes_back() {
    let fixture = Fixture::new();
    let program = fixture.script("hello", "echo hello world", "echo hello world");

    let outcome = fixture.call(json!({ "program": program })).await;
    let envelope = envelope(&outcome);

    assert_eq!(envelope["ok"], true, "{envelope}");
    assert_eq!(envelope["tool"], "shell_exec");
    assert_eq!(envelope["meta"]["exit_code"], 0);
    assert!(
        envelope["content"]
            .as_str()
            .expect("content")
            .contains("hello world"),
        "{envelope}"
    );
    assert_eq!(envelope["truncated"], false);
    assert_eq!(outcome.audit.outcome, Outcome::Ok);
}

#[tokio::test]
async fn arguments_reach_the_program_as_a_vector() {
    let fixture = Fixture::new();
    // Two arguments, the second carrying a space. A layer that joined them
    // into a command line and re-split it would lose the distinction.
    //
    // `%~1` rather than `%1`: cmd hands a batch file its parameters with the
    // quoting intact, and the tilde is how a batch file asks for the value
    // rather than the quoted spelling of it. That is a fact about batch files,
    // not about what was passed.
    let program = fixture.script("args", "echo [%~1] [%~2]", r#"echo "[$1] [$2]""#);

    let outcome = fixture
        .call(json!({ "program": program, "args": ["one", "two three"] }))
        .await;
    let content = envelope(&outcome)["content"]
        .as_str()
        .expect("content")
        .to_owned();

    assert!(content.contains("[one]"), "{content}");
    assert!(content.contains("[two three]"), "{content}");
}

/// A non-zero exit is a result, not a tool failure: `grep` finding nothing and
/// a compiler reporting errors are both answers, and the output is where the
/// answer is.
#[tokio::test]
async fn a_non_zero_exit_is_reported_rather_than_thrown_away() {
    let fixture = Fixture::new();
    let program = fixture.script(
        "failing",
        "echo something went wrong\r\nexit /b 3",
        "echo something went wrong\nexit 3",
    );

    let outcome = fixture.call(json!({ "program": program })).await;
    let envelope = envelope(&outcome);

    assert_eq!(envelope["ok"], true, "the tool ran; the program said no");
    assert_eq!(envelope["meta"]["exit_code"], 3);
    assert!(
        envelope["content"]
            .as_str()
            .expect("content")
            .contains("something went wrong"),
        "the output survives a non-zero exit: {envelope}"
    );
    assert!(outcome.summary.contains("exited 3"), "{}", outcome.summary);
}

#[tokio::test]
async fn stderr_is_captured_and_kept_apart_on_the_way_to_the_ui() {
    let fixture = Fixture::new();
    let program = fixture.script(
        "noisy",
        "echo out\r\necho err 1>&2",
        "echo out\necho err >&2",
    );

    let recorder = Recorder::default();
    let outcome = fixture
        .watched(
            json!({ "program": program }),
            &recorder,
            &CancellationToken::new(),
        )
        .await;

    let content = envelope(&outcome)["content"]
        .as_str()
        .expect("content")
        .to_owned();
    assert!(content.contains("out"), "{content}");
    assert!(content.contains("err"), "{content}");

    assert!(recorder.text_of(Stream::Stdout).contains("out"));
    assert!(recorder.text_of(Stream::Stderr).contains("err"));
}

/// PLAN 4.3: 64 KB of combined output, head and tail, with an elision marker.
#[tokio::test]
async fn a_flood_of_output_is_capped_and_says_so() {
    let fixture = Fixture::new();
    let program = fixture.script(
        "flood",
        &format!("for /L %%i in (1,1,700) do @echo {LONG_LINE}"),
        &format!("i=0\nwhile [ $i -lt 700 ]; do echo {LONG_LINE}; i=$((i+1)); done"),
    );

    let recorder = Recorder::default();
    let outcome = fixture
        .watched(
            json!({ "program": program }),
            &recorder,
            &CancellationToken::new(),
        )
        .await;
    let envelope = envelope(&outcome);
    let content = envelope["content"].as_str().expect("content");

    assert_eq!(envelope["ok"], true, "{envelope}");
    assert_eq!(envelope["truncated"], true, "700 long lines is over 64 KB");
    assert!(
        content.len() <= 64 * 1024 + 64,
        "the envelope carries {} bytes",
        content.len()
    );
    assert!(
        content.contains("bytes elided"),
        "the cut is named in the text"
    );
    assert!(
        envelope["bytes"].as_u64().expect("bytes") > 64 * 1024,
        "the envelope reports what the command produced, not what survived"
    );

    // The pane is bounded too, or a command like this floods the WebView
    // (PLAN 5.4).
    assert!(
        recorder.text().len() <= 64 * 1024 + 16 * 1024,
        "the progress stream is capped as well"
    );
}

/// PLAN 5.4: output is coalesced into frames rather than emitted per read, but
/// it does arrive while the command is still running rather than in one lump
/// at the end.
#[tokio::test]
async fn output_arrives_in_frames_while_the_command_runs() {
    let fixture = Fixture::new();
    let program = fixture.script(
        "chatty",
        "echo one\r\necho two\r\necho three",
        "echo one\necho two\necho three",
    );

    let recorder = Recorder::default();
    fixture
        .watched(
            json!({ "program": program }),
            &recorder,
            &CancellationToken::new(),
        )
        .await;

    assert!(recorder.frames() > 0, "the pane saw the command run");
    let seen = recorder.text();
    for word in ["one", "two", "three"] {
        assert!(seen.contains(word), "{seen}");
    }
}

// ---------------------------------------------------------------------------
// Ending badly
// ---------------------------------------------------------------------------

/// PLAN 4.3: the deadline is real, and it kills the command rather than
/// waiting politely for it.
#[tokio::test]
async fn a_command_that_will_not_finish_is_killed_at_its_deadline() {
    let fixture = Fixture::new();
    let program = fixture.script("slow", "ping -n 60 127.0.0.1 >nul", "sleep 60");

    let started = Instant::now();
    let outcome = fixture
        .call(json!({ "program": program, "timeout_ms": 300 }))
        .await;
    let elapsed = started.elapsed();

    let envelope = envelope(&outcome);
    assert_eq!(envelope["ok"], false, "{envelope}");
    assert_eq!(envelope["error"]["code"], "E_TIMEOUT");
    assert!(
        elapsed < Duration::from_secs(30),
        "the deadline is what ended it, after {elapsed:?}"
    );
    assert_eq!(outcome.audit.outcome, Outcome::Error);
}

/// A Stop that lands while a command is running has to stop the command, or it
/// is a button with no effect the user can see.
#[tokio::test]
async fn cancelling_a_turn_kills_the_command() {
    let fixture = Fixture::new();
    let program = fixture.script("slow", "ping -n 60 127.0.0.1 >nul", "sleep 60");

    let cancel = CancellationToken::new();
    let stopper = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        stopper.cancel();
    });

    let started = Instant::now();
    let outcome = fixture
        .watched(json!({ "program": program }), &Recorder::default(), &cancel)
        .await;
    let elapsed = started.elapsed();

    let envelope = envelope(&outcome);
    assert_eq!(envelope["ok"], false, "{envelope}");
    assert_eq!(envelope["error"]["code"], "E_CANCELLED");
    assert!(
        elapsed < Duration::from_secs(30),
        "the cancel is what ended it, after {elapsed:?}"
    );
    assert_eq!(
        outcome.audit.outcome,
        Outcome::Cancelled,
        "a stopped command is not a failed one"
    );
}

/// Windows console programs write the system OEM code page, not UTF-8, so a
/// French `dir` listing puts `numéro` on the pipe as `num\x82ro`. Reading that
/// as UTF-8 turns every accent into a replacement character, which is what a
/// user sees first and what makes the pane look broken.
#[cfg(windows)]
#[tokio::test]
async fn console_output_is_decoded_rather_than_mangled() {
    let fixture = Fixture::new();

    // Written as bytes, because the point is a byte no UTF-8 decoder accepts:
    // `0x82` is `é` in code pages 437 and 850. `cmd` echoes what is in the
    // file, so this is the same shape as a real listing.
    let path = fixture.workspace.join("accents.cmd");
    fs::write(&path, b"@echo off\r\necho num\x82ro\r\n").expect("write");

    let outcome = fixture.call(json!({ "program": "./accents.cmd" })).await;
    let content = envelope(&outcome)["content"]
        .as_str()
        .expect("content")
        .to_owned();

    assert!(
        !content.contains('\u{fffd}'),
        "the OEM byte was decoded, not replaced: {content:?}"
    );
    assert!(content.contains("num"), "{content:?}");
    assert!(
        content.trim().chars().count() == 6,
        "one accented word, six characters: {content:?}"
    );
}

/// A command that colours its output is showing it to a terminal. Aegis'
/// transcript is not one, so the sequences are dropped — from what the model
/// reads and from what the pane shows alike.
#[tokio::test]
async fn colour_from_a_real_command_never_reaches_the_transcript() {
    let fixture = Fixture::new();
    let program = fixture.script(
        "colour",
        "echo \x1b[36mcyan word \x1b[0m",
        "printf '\x1b[36mcyan word \x1b[0m
'",
    );

    let recorder = Recorder::default();
    let outcome = fixture
        .watched(
            json!({ "program": program }),
            &recorder,
            &CancellationToken::new(),
        )
        .await;

    let content = envelope(&outcome)["content"]
        .as_str()
        .expect("content")
        .to_owned();

    assert!(
        content.contains("cyan word"),
        "the words survive: {content:?}"
    );
    for shown in [&content, &recorder.text()] {
        assert!(
            !shown.contains('\x1b'),
            "no escape reaches a reader: {shown:?}"
        );
        assert!(
            !shown.contains("[36m"),
            "and no half of one either: {shown:?}"
        );
    }
}

#[tokio::test]
async fn a_program_that_is_nowhere_is_named_in_the_answer() {
    let fixture = Fixture::new();

    let outcome = fixture
        .call(json!({ "program": "definitely-not-a-real-program-9k2" }))
        .await;
    let envelope = envelope(&outcome);

    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "E_TOOL_FAILED");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .expect("a message")
            .contains("definitely-not-a-real-program-9k2"),
        "{envelope}"
    );
}

#[test]
fn an_empty_program_is_refused_before_anything_is_resolved() {
    let fixture = Fixture::new();

    match fixture.judge(&json!({ "program": "   " })) {
        Decision::Deny { reason, .. } => assert!(reason.contains("no program"), "{reason}"),
        other => panic!("expected a denial, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// The exit criterion of Phase 7 (PLAN § 6)
// ---------------------------------------------------------------------------

/// A command runs under approval, its output streams while it runs, and the
/// audit log has the line — with the program and the working directory kept
/// whole, because those are what a person reading the log afterwards is
/// looking for.
#[tokio::test]
async fn a_command_runs_under_approval_and_lands_in_the_audit_log() {
    let fixture = Fixture::new();
    let program = fixture.script("greet", "echo hello from aegis", "echo hello from aegis");

    // Policy asks. Nothing runs until that ask is answered.
    assert!(matches!(
        fixture.judge(&json!({ "program": program })),
        Decision::Ask { .. }
    ));

    let recorder = Recorder::default();
    let outcome = fixture
        .watched(
            json!({ "program": program, "args": [] }),
            &recorder,
            &CancellationToken::new(),
        )
        .await;

    assert!(outcome.result.ok, "{:?}", outcome.result.error);
    assert!(
        recorder.text().contains("hello from aegis"),
        "the output was streamed as it was produced: {:?}",
        recorder.text()
    );

    let lines = fixture.audit_lines();
    assert_eq!(lines.len(), 1, "one line per call, whatever the outcome");

    let line = &lines[0];
    assert_eq!(line["tool"], "shell_exec");
    assert_eq!(line["decision"], "allow_once");
    assert_eq!(line["outcome"], "ok");
    assert_eq!(line["error_code"], Value::Null);
    assert!(
        line["args_redacted"]
            .as_str()
            .expect("redacted args")
            .contains("greet"),
        "the program is kept whole in the log: {line}"
    );
    assert!(
        line["duration_ms"].as_u64().is_some(),
        "how long it took is part of the record"
    );
}
