//! The MCP client, against a real server on the other end of a real pipe
//! (PLAN 7.3, Phase 18).
//!
//! Phase 18's exit condition:
//! [`the_exit_condition_a_connector_call_is_gated_like_a_write`].
//!
//! **The server is this binary**: [`mock_mcp_server`] is an `#[ignore]`d test
//! speaking MCP on stdio, spawned via `current_exe()`. No Node/Python
//! dependency, no shipped mock binary, and the real transport is exercised.

use std::io::{BufRead as _, Write as _};
use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::{json, Value};
use tempfile::TempDir;

use aegis_lib::agent::event::EventSink;
use aegis_lib::agent::turn::TurnPlan;
use aegis_lib::{
    Agent, ApprovalDecision, ApprovalRegistry, ApprovalRequest, AuditLog, ConnectorState,
    Connectors, Event, FakeProvider, Grant, GrantStore, MemoryStore, ModelEvent, Outcome,
    SessionState, SessionStore, Standing, StopReason, ToolCallStatus, Turn, TurnRegistry,
    DEFAULT_AGENT_ID,
};
use aegis_lib::{Connector, ResolvedCall};

// ---------------------------------------------------------------------------
// The server
// ---------------------------------------------------------------------------

/// The name the tests filter on when they re-enter this binary.
const SERVER_TEST: &str = "mock_mcp_server";

/// Not a test: the MCP server the other tests in this file talk to.
///
/// Spawned by name. Uses `io::stdout()`, not `println!`, which libtest's
/// capture would swallow.
#[test]
#[ignore = "not a test: this is the MCP server the other tests spawn"]
fn mock_mcp_server() {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();

    // libtest prints `test mock_mcp_server ... ` with no trailing newline
    // before it runs the test, so the first frame would otherwise share a line
    // with it and the client would read that line as junk — which it would be.
    // One newline closes it. Nothing else this file writes to stdout is not a
    // frame.
    let _ = writeln!(out);
    let _ = out.flush();

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(frame) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(method) = frame.get("method").and_then(Value::as_str) else {
            continue;
        };
        // A notification: nothing is answered, which is the half of JSON-RPC a
        // client that waited on `initialized` would hang on.
        let Some(id) = frame.get("id").cloned() else {
            continue;
        };

        let result = match method {
            "initialize" => json!({
                "protocolVersion": "2025-06-18",
                "capabilities": { "tools": { "listChanged": true } },
                "serverInfo": { "name": "mock", "version": "0.1.0" },
            }),
            "tools/list" => json!({
                "tools": [
                    {
                        "name": "status",
                        "description": "Says what the mock server thinks is going on.",
                        "inputSchema": { "type": "object", "properties": {} },
                        "annotations": { "readOnlyHint": true },
                    },
                    {
                        "name": "write_note",
                        "description": "Pretends to write something down.",
                        "inputSchema": {
                            "type": "object",
                            "properties": { "text": { "type": "string" } },
                            "required": ["text"],
                        },
                    },
                ],
            }),
            "tools/call" => {
                let tool = frame
                    .pointer("/params/name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let arguments = frame
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));

                match tool.as_str() {
                    "status" => json!({
                        "content": [{ "type": "text", "text": "the mock server is fine" }],
                    }),
                    "write_note" => match arguments.get("text").and_then(Value::as_str) {
                        Some(text) => json!({
                            "content": [{ "type": "text", "text": format!("noted: {text}") }],
                        }),
                        // The tool failing on its own terms, which is a
                        // different thing from the connector failing.
                        None => json!({
                            "isError": true,
                            "content": [{ "type": "text", "text": "write_note needs `text`" }],
                        }),
                    },
                    other => json!({
                        "isError": true,
                        "content": [{ "type": "text", "text": format!("no tool `{other}`") }],
                    }),
                }
            }
            _ => {
                let refusal = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": format!("no method `{method}`") },
                });
                let _ = writeln!(out, "{refusal}");
                let _ = out.flush();
                continue;
            }
        };

        let answer = json!({ "jsonrpc": "2.0", "id": id, "result": result });
        let _ = writeln!(out, "{answer}");
        let _ = out.flush();
    }
}

/// A connector record pointing at [`mock_mcp_server`].
fn mock(id: &str) -> Connector {
    let exe = std::env::current_exe().expect("this test binary");
    Connector {
        id: id.to_owned(),
        name: "Mock".to_owned(),
        command: exe.display().to_string(),
        args: vec![
            SERVER_TEST.to_owned(),
            "--exact".to_owned(),
            "--ignored".to_owned(),
            "--nocapture".to_owned(),
            "--test-threads=1".to_owned(),
        ],
        env: Vec::new(),
        enabled: true,
    }
}

/// A connector record pointing at a program that is not a server at all.
///
/// The same binary with a filter that selects nothing: it starts, prints that
/// it ran no tests, and exits. Which is exactly the failure a person hits when
/// they type a package name wrong.
fn not_a_server(id: &str) -> Connector {
    let mut connector = mock(id);
    connector.args = vec!["there_is_no_such_test".to_owned(), "--exact".to_owned()];
    connector
}

// ---------------------------------------------------------------------------
// The roster
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_connector_starts_and_its_tools_arrive_named_for_it() {
    let roster = Connectors::new();
    let view = roster.connect(&mock("mock")).await;

    assert_eq!(view.state, ConnectorState::Ready, "{:?}", view.error);
    assert_eq!(view.server.as_deref(), Some("mock 0.1.0"));
    assert_eq!(view.protocol.as_deref(), Some("2025-06-18"));

    let names: Vec<&str> = view
        .tools
        .iter()
        .map(|tool| tool.full_name.as_str())
        .collect();
    assert_eq!(names, vec!["mock__status", "mock__write_note"]);

    // The catalog is what the turn loop reads, and it carries the same names.
    let catalog = roster.catalog();
    assert!(catalog.find("mock__status").is_some());
    assert!(
        catalog.find("status").is_none(),
        "the bare name is not a tool"
    );

    // The server's own claim about `status` is carried, and it is carried as a
    // claim: it lands in a field named after what it is.
    assert!(catalog.find("mock__status").expect("there").read_only_hint);
    assert!(
        !catalog
            .find("mock__write_note")
            .expect("there")
            .read_only_hint
    );

    roster.shutdown().await;
}

#[tokio::test]
async fn a_program_that_is_not_a_server_is_a_row_that_says_so() {
    let roster = Connectors::new();
    let view = roster.connect(&not_a_server("broken")).await;

    assert_eq!(view.state, ConnectorState::Failed);
    assert!(view.error.is_some(), "a failed connector says why");
    assert!(view.tools.is_empty());
    // And it offers nothing to anybody.
    assert!(roster.catalog().is_empty());

    roster.shutdown().await;
}

#[tokio::test]
async fn a_disconnected_connector_offers_nothing_and_refuses_a_call() {
    let roster = Connectors::new();
    roster.connect(&mock("mock")).await;
    assert!(!roster.catalog().is_empty());

    roster.disconnect("mock").await;
    assert!(roster.catalog().is_empty());

    let refused = roster
        .call("mock", "status", &json!({}))
        .await
        .expect_err("nothing answers");
    assert!(refused.contains("Settings"), "{refused}");
}

/// Holding one tool of a connector holds none of the others, and that is the
/// whole reason the allow-list keys on the full name.
#[tokio::test]
async fn an_identity_is_offered_only_the_connector_tools_it_holds() {
    let roster = Connectors::new();
    roster.connect(&mock("mock")).await;
    let catalog = roster.catalog();

    let held = vec!["fs_read".to_owned(), "mock__status".to_owned()];
    let schemas = aegis_lib::tools::schemas_for(&held, &catalog);

    let names: Vec<String> = schemas
        .iter()
        .map(|schema| schema["function"]["name"].as_str().unwrap_or("").to_owned())
        .collect();
    assert_eq!(names, vec!["fs_read", "mock__status"]);

    roster.shutdown().await;
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

/// Collects every event a turn emits.
#[derive(Debug, Default)]
struct Recorder {
    events: Mutex<Vec<Event>>,
}

impl Recorder {
    fn events(&self) -> Vec<Event> {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn asked(&self) -> Vec<ApprovalRequest> {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                Event::ToolApprovalRequired(request) => Some(*request),
                _ => None,
            })
            .collect()
    }

    /// How each call ended, and the one line the transcript shows for it.
    ///
    /// `tool:finished` carries the outcome rather than the tool's name — the
    /// name is on the audit line and on the transcript record, both of which
    /// these tests also read.
    fn finished(&self) -> Vec<(Outcome, String)> {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                Event::ToolFinished(done) => Some((done.outcome, done.summary)),
                _ => None,
            })
            .collect()
    }
}

impl EventSink for Recorder {
    fn emit(&self, event: Event) {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(event);
    }
}

/// A data directory, a workspace and the stores a turn needs.
struct App {
    _dir: TempDir,
    workspace: PathBuf,
    sessions: SessionStore,
    turns: TurnRegistry,
    grants: GrantStore,
    approvals: ApprovalRegistry,
    audit: AuditLog,
    session_id: String,
    captures: PathBuf,
    library: PathBuf,
    memories: MemoryStore,
}

impl App {
    fn new() -> Self {
        let dir = TempDir::new().expect("temp dir");
        let data = dir.path().join("data");
        let workspace = dir.path().join("work");
        std::fs::create_dir_all(&data).expect("data dir");
        std::fs::create_dir_all(&workspace).expect("workspace dir");

        let sessions = SessionStore::load(&data);
        let session_id = sessions
            .create("project-1", None, DEFAULT_AGENT_ID)
            .expect("session")
            .id;

        Self {
            workspace: dunce::canonicalize(&workspace).expect("canonical workspace"),
            _dir: dir,
            sessions,
            turns: TurnRegistry::new(),
            grants: GrantStore::new(),
            approvals: ApprovalRegistry::new(),
            audit: AuditLog::new(&data),
            session_id,
            captures: data.join("captures"),
            library: data.join("skills"),
            memories: MemoryStore::load(&data),
        }
    }

    /// Runs one turn of scripted tool calls, answering each approval it raises
    /// with `answers` in order.
    ///
    /// The two halves run concurrently, the way the application has them: the
    /// turn is a task, and the answers arrive from a window.
    async fn turn(
        &self,
        turn_id: &str,
        agent: &Agent,
        connectors: &Connectors,
        sink: &Recorder,
        rounds: Vec<(&str, Value)>,
        answers: &[ApprovalDecision],
    ) -> StopReason {
        let cancel = self.turns.begin(&self.session_id, turn_id).expect("free");

        let script: Vec<Vec<ModelEvent>> = rounds
            .into_iter()
            .enumerate()
            .map(|(index, (name, args))| {
                vec![
                    ModelEvent::ToolCallDelta {
                        index: 0,
                        id: Some(format!("call_{index}")),
                        name: Some(name.to_owned()),
                        args_delta: args.to_string(),
                        thought_signature: None,
                    },
                    ModelEvent::Finish {
                        reason: StopReason::ToolCalls,
                        usage: None,
                    },
                ]
            })
            .collect();
        let provider = FakeProvider::scripted(script);

        let plan = TurnPlan {
            session_id: self.session_id.clone(),
            turn_id: turn_id.to_owned(),
            workspace: Some(self.workspace.clone()),
            exec_host: None,
        };
        let turn = Turn {
            agent,
            sessions: &self.sessions,
            turns: &self.turns,
            grants: &self.grants,
            approvals: &self.approvals,
            audit: &self.audit,
            provider: &provider,
            sink,
            self_exe: None,
            captures: &self.captures,
            skills: &self.library,
            memories: &self.memories,
            connectors,
            standing: Standing::Own(None),
            unattended: None,
        };
        let running = turn.run(&plan, &cancel);

        let answering = async {
            for decision in answers {
                let request = self.next_request().await;
                self.approvals
                    .resolve(&request.request_id, *decision, &self.grants)
                    .expect("resolvable");
            }
        };

        let (reason, ()) = tokio::join!(running, answering);
        self.turns
            .finish(&self.session_id, turn_id, SessionState::Idle);
        reason
    }

    /// The approval the turn is currently blocked on.
    async fn next_request(&self) -> ApprovalRequest {
        for _ in 0..2000 {
            if let Some(request) = self
                .approvals
                .list(Some(&self.session_id))
                .into_iter()
                .next()
            {
                return request;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("no approval was ever raised");
    }
}

/// **The Phase 18 exit condition.** A tool that lives in another process is
/// callable under the same approval dialog as `fs_write`: the model asks, a
/// person reads what would happen and says yes, the call runs, and the audit
/// log carries a line for it under the name the model used.
#[tokio::test]
async fn the_exit_condition_a_connector_call_is_gated_like_a_write() {
    let app = App::new();
    let roster = Connectors::new();
    let ready = roster.connect(&mock("mock")).await;
    assert_eq!(ready.state, ConnectorState::Ready, "{:?}", ready.error);

    let sink = Recorder::default();
    let agent = Agent::builtin();
    let reason = app
        .turn(
            "turn-1",
            &agent,
            &roster,
            &sink,
            vec![("mock__status", json!({}))],
            &[ApprovalDecision::AllowOnce],
        )
        .await;
    // The scripted round asked for the tool; the round after it had nothing
    // left to script, so the turn ends the way any turn does.
    assert_eq!(reason, StopReason::Stop);

    // A person was asked, and what they were shown names the connector, the
    // tool and the server's own description of it.
    let asked = sink.asked();
    assert_eq!(asked.len(), 1, "exactly one dialog");
    let request = &asked[0];
    assert_eq!(request.tool, "mock__status");
    assert_eq!(request.title, "Call a connector");
    assert!(request.summary.contains("Mock"), "{}", request.summary);
    assert!(request.session_grant_allowed, "a grant is on offer");
    match &request.detail {
        aegis_lib::policy::ApprovalDetail::Connector {
            connector,
            tool,
            description,
            read_only_hint,
            arguments,
            ..
        } => {
            assert_eq!(connector, "mock");
            assert_eq!(tool, "status");
            assert!(description.contains("mock server"), "{description}");
            assert!(*read_only_hint, "the server's claim is carried");
            assert_eq!(arguments, "{}");
        }
        other => panic!("the dialog drew the wrong thing: {other:?}"),
    }

    // The call ran, and what came back is the server's text.
    let finished = sink.finished();
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0].0, Outcome::Ok);

    let messages = app.sessions.messages(&app.session_id).expect("messages");
    let said = messages
        .iter()
        .filter(|message| message.text.contains("the mock server is fine"))
        .count();
    assert_eq!(said, 1, "the answer reached the transcript");
    // And it reads as the tool it was, under the name the model used.
    let record = messages
        .iter()
        .flat_map(|message| message.tool_calls.iter())
        .find(|call| call.tool == "mock__status")
        .expect("a tool record");
    assert_eq!(record.status, ToolCallStatus::Ok);

    // And there is one audit line, under the name the model used.
    let lines = app.audit.tail(50, None).expect("tail");
    let line = lines
        .iter()
        .find(|line| line.tool == "mock__status")
        .expect("an audit line for the connector call");
    assert_eq!(line.outcome, Outcome::Ok);
    assert_eq!(line.session_id, app.session_id);

    roster.shutdown().await;
}

/// Denying one is a result, not an exception: the model is told, and the turn
/// carries on (PLAN 4.3).
#[tokio::test]
async fn denying_a_connector_call_leaves_the_turn_running() {
    let app = App::new();
    let roster = Connectors::new();
    roster.connect(&mock("mock")).await;

    let sink = Recorder::default();
    let agent = Agent::builtin();
    app.turn(
        "turn-1",
        &agent,
        &roster,
        &sink,
        vec![("mock__write_note", json!({ "text": "hello" }))],
        &[ApprovalDecision::Deny],
    )
    .await;

    let finished = sink.finished();
    assert_eq!(finished[0].0, Outcome::Denied);

    let lines = app.audit.tail(50, None).expect("tail");
    let line = lines
        .iter()
        .find(|line| line.tool == "mock__write_note")
        .expect("a line for the refusal");
    assert_eq!(line.outcome, Outcome::Denied);

    roster.shutdown().await;
}

/// A session grant covers the tool it was granted on, and nothing else the
/// same connector offers. That is the property the whole phase keys on: a
/// server may add a tool at any time, and it must not arrive pre-approved.
#[tokio::test]
async fn a_grant_covers_one_tool_and_not_the_connector() {
    let app = App::new();
    let roster = Connectors::new();
    roster.connect(&mock("mock")).await;

    let sink = Recorder::default();
    let agent = Agent::builtin();

    // Allowed for the session on the first tool …
    app.turn(
        "turn-1",
        &agent,
        &roster,
        &sink,
        vec![("mock__status", json!({}))],
        &[ApprovalDecision::AllowSession],
    )
    .await;

    let held = app.grants.list(&app.session_id);
    assert_eq!(
        held,
        vec![Grant::Connector {
            tool: "mock__status".to_owned()
        }]
    );
    assert!(
        held[0].scope_label().contains("nothing it adds later"),
        "{}",
        held[0].scope_label()
    );

    // … the same tool again does not ask …
    let second = Recorder::default();
    app.turn(
        "turn-2",
        &agent,
        &roster,
        &second,
        vec![("mock__status", json!({}))],
        &[],
    )
    .await;
    assert!(second.asked().is_empty(), "the grant covered it");
    assert_eq!(second.finished()[0].0, Outcome::Ok);

    // … and the other tool of the same connector still does.
    let third = Recorder::default();
    app.turn(
        "turn-3",
        &agent,
        &roster,
        &third,
        vec![("mock__write_note", json!({ "text": "x" }))],
        &[ApprovalDecision::AllowOnce],
    )
    .await;
    assert_eq!(third.asked().len(), 1, "a different tool asks again");

    roster.shutdown().await;
}

/// A tool that fails on its own terms is not a connector that failed, and the
/// model is told which.
#[tokio::test]
async fn a_tool_error_is_an_envelope_and_the_connector_stays_up() {
    let app = App::new();
    let roster = Connectors::new();
    roster.connect(&mock("mock")).await;

    let sink = Recorder::default();
    let agent = Agent::builtin();
    app.turn(
        "turn-1",
        &agent,
        &roster,
        &sink,
        // No `text`, which the mock server answers with `isError`.
        vec![("mock__write_note", json!({}))],
        &[ApprovalDecision::AllowOnce],
    )
    .await;

    let finished = sink.finished();
    assert_eq!(finished[0].0, Outcome::Error);
    assert!(finished[0].1.contains("needs `text`"), "{}", finished[0].1);

    // The connector is still there, and still offering both tools.
    assert_eq!(roster.catalog().tools().len(), 2);

    roster.shutdown().await;
}

/// An identity holds connector tools by name, like any other tool, and one it
/// was not granted is refused without a dialog.
#[tokio::test]
async fn a_connector_tool_outside_the_allow_list_is_refused_by_name() {
    let app = App::new();
    let roster = Connectors::new();
    roster.connect(&mock("mock")).await;

    let reviewer = Agent {
        id: "agent-reviewer".to_owned(),
        name: "Reviewer".to_owned(),
        role: "reads".to_owned(),
        instructions: String::new(),
        provider_id: "default".to_owned(),
        model: String::new(),
        tools: vec!["fs_read".to_owned(), "mock__status".to_owned()],
        skills: Vec::new(),
        runs_per_day: 0,
        builtin: false,
    };

    let sink = Recorder::default();
    app.turn(
        "turn-1",
        &reviewer,
        &roster,
        &sink,
        vec![("mock__write_note", json!({ "text": "x" }))],
        &[],
    )
    .await;

    assert!(
        sink.asked().is_empty(),
        "no dialog for a tool it cannot use"
    );
    let finished = sink.finished();
    assert_eq!(finished[0].0, Outcome::Denied);
    assert!(finished[0].1.contains("Reviewer"), "{}", finished[0].1);

    roster.shutdown().await;
}

/// A name nothing answers to is refused rather than asked about: an approval
/// for a tool that cannot run could not mean anything.
#[test]
fn a_connector_tool_nobody_offers_is_refused_without_a_dialog() {
    let grants = GrantStore::new();
    let workspace = std::env::current_dir().expect("a workspace");
    let catalog = aegis_lib::ConnectorCatalog::empty();
    let ctx =
        aegis_lib::PolicyCtx::new("s1", Some(&workspace), &grants).with_connectors(Some(&catalog));

    match aegis_lib::policy::decide(&ctx, "git__status", json!({})) {
        aegis_lib::Decision::Deny { reason, .. } => {
            assert!(reason.contains("git"), "{reason}");
            assert!(reason.contains("Settings"), "{reason}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }

    // A name that is not shaped like a connector's is still just unknown.
    match aegis_lib::policy::decide(&ctx, "rm_rf", json!({})) {
        aegis_lib::Decision::Deny { reason, .. } => assert!(reason.contains("unknown tool")),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// The arguments the dialog shows are the arguments that are sent. There is
/// nothing between them, and the type says so.
#[test]
fn what_the_dialog_shows_is_what_is_sent() {
    let parsed = aegis_lib::ToolCall::parse("git__status", json!({ "path": "..", "n": 2 }))
        .expect("a connector call");
    match parsed {
        aegis_lib::ToolCall::Connector { name, args } => {
            assert_eq!(name, "git__status");
            assert_eq!(args["path"], "..");
            assert_eq!(args["n"], 2);
        }
        other => panic!("expected a connector call, got {other:?}"),
    }

    // And a resolved one carries the same name dispatch and the log will use.
    let call = ResolvedCall::Connector {
        name: "git__status".to_owned(),
        args: json!({}),
    };
    assert_eq!(call.tool(), "git__status");
}
