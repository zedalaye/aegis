//! `screen_capture`, driven the way the turn loop drives it.
//!
//! Every test goes `policy::decide` → `tools::run` or `tools::refuse`.
//!
//! A capture either yields one PNG whose digest matches the audit line, or
//! `E_SCREEN_PERMISSION` with no file — so the suite passes on desktops,
//! unauthorized macOS and headless runners alike. Always asserted: the gate
//! asks, refusals write nothing, the audit line has no image, and a session
//! grant covers later captures.

use std::fs;
use std::path::PathBuf;

use aegis_lib::audit::{AuditDecision, AuditLog, Outcome};
use aegis_lib::policy::{decide, tool, Decision, Grant, GrantStore, PolicyCtx, Risk};
use aegis_lib::tools::{self, NullProgress, ToolCtx, ToolOutcome};
use aegis_lib::HandoffCtx;
use aegis_lib::{MemoryStore, SkillCtx, DEFAULT_AGENT_ID};
use serde_json::{json, Value};
use tempfile::TempDir;

/// A capture directory, a workspace, the grants and a fresh audit log.
///
/// The workspace exists only because policy requires one (PLAN 3.2).
struct Fixture {
    _dir: TempDir,
    workspace: PathBuf,
    captures: PathBuf,
    /// An empty memory store. Nothing here remembers anything; the store is
    /// only there because a tool call is not runnable without one.
    memories: MemoryStore,
    grants: GrantStore,
    audit: AuditLog,
    runtime: tokio::runtime::Runtime,
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new().expect("temp dir");
        let data = dir.path().join("data");
        let workspace = dir.path().join("work");
        fs::create_dir_all(&data).expect("data dir");
        fs::create_dir_all(&workspace).expect("workspace dir");

        Self {
            workspace: dunce::canonicalize(&workspace).expect("canonical workspace"),
            captures: data.join("captures"),
            memories: MemoryStore::load(&data),
            audit: AuditLog::new(&data),
            _dir: dir,
            grants: GrantStore::new(),
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a tokio runtime"),
        }
    }

    fn policy(&self) -> PolicyCtx<'_> {
        PolicyCtx::new("session-1", Some(&self.workspace), &self.grants)
    }

    /// What policy says about a capture, without carrying it out.
    fn judge(&self, args: Value) -> Decision {
        decide(&self.policy(), tool::SCREEN_CAPTURE, args)
    }

    /// One call end to end: policy decides, and whatever it decided happens and
    /// is audited. An ask is answered the way a click on "Allow once" answers
    /// it.
    fn call(&self, args: Value) -> ToolOutcome {
        let cancel = tokio_util::sync::CancellationToken::new();
        let ctx = ToolCtx {
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

        match self.judge(args.clone()) {
            Decision::Auto { call, reason } => {
                self.runtime
                    .block_on(tools::run(&ctx, AuditDecision::Auto, reason, &call))
            }
            Decision::Ask { call, request } => self.runtime.block_on(tools::run(
                &ctx,
                AuditDecision::AllowOnce,
                &request.reason,
                &call,
            )),
            Decision::Deny { code, reason } => tools::refuse(
                &ctx,
                tool::SCREEN_CAPTURE,
                AuditDecision::Deny,
                code,
                &reason,
            ),
        }
    }

    /// A user answering "Deny", which is what the turn loop does with the same
    /// decision.
    fn deny(&self, args: Value) -> ToolOutcome {
        let cancel = tokio_util::sync::CancellationToken::new();
        let ctx = ToolCtx {
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

        tools::refuse(
            &ctx,
            tool::SCREEN_CAPTURE,
            AuditDecision::Deny,
            aegis_lib::ErrorCode::Denied,
            "the user refused this capture",
        )
    }

    /// The PNGs written so far.
    fn written(&self) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(&self.captures) else {
            return Vec::new();
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect();
        paths.sort();
        paths
    }

    /// Every audit line written so far, oldest first.
    fn audit_lines(&self) -> Vec<Value> {
        let text = fs::read_to_string(self.audit.path()).unwrap_or_default();
        text.lines()
            .map(|line| serde_json::from_str(line).expect("every line is JSON"))
            .collect()
    }
}

/// The envelope as the model would receive it.
fn envelope(outcome: &ToolOutcome) -> Value {
    serde_json::from_str(&outcome.result.to_json()).expect("the envelope is valid JSON")
}

/// SHA-256 of a file, hex — the same digest the audit line carries.
fn digest_of(path: &PathBuf) -> String {
    use sha2::{Digest as _, Sha256};

    let bytes = fs::read(path).expect("the capture is readable");
    Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

/// The row exists to ask, always. There is no argument, no grant and no state
/// that turns a capture into something that happens without a person.
#[test]
fn a_capture_is_never_taken_without_asking() {
    let fixture = Fixture::new();

    for args in [json!({}), json!({ "display": "primary" })] {
        match fixture.judge(args.clone()) {
            Decision::Ask { request, .. } => {
                assert_eq!(request.tool, tool::SCREEN_CAPTURE);
                assert_eq!(request.risk, Risk::Medium);
                assert_eq!(request.grant, Some(Grant::ScreenCapture));
                assert!(
                    request.reason.contains("every window"),
                    "the prompt says what a capture contains: {}",
                    request.reason
                );
            }
            other => panic!("expected an ask for {args}, got {other:?}"),
        }
    }

    assert!(fixture.written().is_empty(), "deciding captures nothing");
}

#[test]
fn a_display_this_build_cannot_capture_is_refused_before_anything_runs() {
    let fixture = Fixture::new();

    let outcome = fixture.call(json!({ "display": "hdmi-2" }));
    let envelope = envelope(&outcome);

    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "E_TOOL_FAILED");
    assert_eq!(outcome.audit.outcome, Outcome::Denied);
    assert!(fixture.written().is_empty());
}

#[test]
fn a_refused_capture_writes_nothing_and_says_so_in_the_log() {
    let fixture = Fixture::new();

    let outcome = fixture.deny(json!({}));
    let envelope = envelope(&outcome);

    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "E_DENIED");
    assert_eq!(outcome.image_path, None);
    assert!(fixture.written().is_empty(), "nothing reached the disk");

    let line = fixture.audit_lines().pop().expect("one line");
    assert_eq!(line["tool"], "screen_capture");
    assert_eq!(line["decision"], "deny");
    assert_eq!(line["outcome"], "denied");
    assert_eq!(line["artifact"], Value::Null, "a refusal has no artefact");
}

/// A grant collapses the second ask and not the first: the user approved
/// capturing for the session, and that is a decision the store remembers rather
/// than one policy re-derives (PLAN 3.1).
#[test]
fn allowing_captures_for_the_session_stops_the_prompt() {
    let fixture = Fixture::new();

    assert!(
        matches!(fixture.judge(json!({})), Decision::Ask { .. }),
        "the first capture asks"
    );

    fixture.grants.insert("session-1", Grant::ScreenCapture);

    match fixture.judge(json!({})) {
        Decision::Auto { call, .. } => assert_eq!(call.tool(), tool::SCREEN_CAPTURE),
        other => panic!("expected the grant to cover it, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// The capture itself
// ---------------------------------------------------------------------------

/// The exit criterion of Phase 9 (PLAN § 6), as far as a test can state it.
///
/// A matching PNG, or `E_SCREEN_PERMISSION` and nothing — never a blank
/// screenshot reported as success (PLAN 5.2).
#[test]
fn a_capture_either_writes_one_png_or_explains_why_not() {
    let fixture = Fixture::new();

    let outcome = fixture.call(json!({}));
    let envelope = envelope(&outcome);
    let line = fixture.audit_lines().pop().expect("one line");

    assert_eq!(line["tool"], "screen_capture");
    assert_eq!(
        line["decision"], "allow_once",
        "the log records that a person allowed it"
    );

    if envelope["ok"] == json!(true) {
        let written = fixture.written();
        assert_eq!(written.len(), 1, "exactly one file: {written:?}");
        let path = &written[0];

        // What the model is told, and what is on disk, are the same file.
        assert_eq!(envelope["meta"]["path"], path.display().to_string());
        assert_eq!(
            outcome.image_path.as_deref(),
            Some(path.display().to_string()).as_deref()
        );
        assert!(
            envelope["meta"]["width"].as_u64().unwrap_or(0) > 0
                && envelope["meta"]["height"].as_u64().unwrap_or(0) > 0
        );

        // The envelope carries a description, never the picture.
        let rendered = outcome.result.to_json();
        assert!(rendered.len() < 4096, "an envelope, not an image");
        assert!(!rendered.contains("data:image"), "{rendered}");

        // And the audit line identifies it (PLAN 5.4).
        assert_eq!(line["outcome"], "ok");
        assert_eq!(line["artifact"]["path"], path.display().to_string());
        assert_eq!(line["artifact"]["sha256"], digest_of(path));
        assert_eq!(line["artifact"]["width"], envelope["meta"]["width"]);
        assert_eq!(line["artifact"]["height"], envelope["meta"]["height"]);
    } else {
        assert_eq!(
            envelope["error"]["code"], "E_SCREEN_PERMISSION",
            "a capture that does not happen says why: {envelope}"
        );
        assert!(
            fixture.written().is_empty(),
            "a refused capture leaves nothing behind"
        );
        assert_eq!(outcome.image_path, None);
        assert_eq!(line["outcome"], "error");
        assert_eq!(line["artifact"], Value::Null);
    }
}

/// Captures are Aegis' own artefacts. One landing in the workspace would end up
/// in somebody's next commit (PLAN 5.4).
#[test]
fn a_capture_never_lands_in_the_workspace() {
    let fixture = Fixture::new();

    let outcome = fixture.call(json!({}));

    if let Some(path) = &outcome.image_path {
        let path = PathBuf::from(path);
        assert!(path.starts_with(&fixture.captures));
        assert!(!path.starts_with(&fixture.workspace));
    }
    assert_eq!(
        fs::read_dir(&fixture.workspace)
            .expect("the workspace is readable")
            .count(),
        0,
        "the workspace is untouched either way"
    );
}

/// The dialog is given the display's own name and both of its sizes, so a user
/// can check the prompt against the screen in front of them (PLAN 5.1).
///
/// Skipped without a display.
#[test]
fn the_prompt_describes_the_display_it_would_capture() {
    let Some(screen) = aegis_lib::tools::screenshot::geometry() else {
        return;
    };

    assert!(!screen.display.trim().is_empty());
    assert!(screen.width > 0 && screen.height > 0);
    assert!(screen.logical_width > 0 && screen.logical_height > 0);

    let fixture = Fixture::new();
    let ctx = fixture.policy().with_screen(Some(&screen));

    match decide(&ctx, tool::SCREEN_CAPTURE, json!({})) {
        Decision::Ask { request, .. } => {
            assert!(
                request.summary.contains(&screen.width.to_string()),
                "the summary names the size: {}",
                request.summary
            );
            assert!(
                request.summary.contains(&screen.display),
                "the summary names the display: {}",
                request.summary
            );
        }
        other => panic!("expected an ask, got {other:?}"),
    }
}
