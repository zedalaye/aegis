//! The decision model (PLAN 7.18), against a loopback stand-in for TypeSafe.
//!
//! No live key and no network: every request lands on `127.0.0.1`. Covered:
//! the client's wire (bearer, path, map-shaped questions, structured state),
//! its failures and retries; `tool_risk` annotating a dialog without ever
//! changing it; `jev_ask` and `jev_eval` through policy and dispatch; and the
//! proposal → `eval.yml` apply.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use aegis_lib::agent::decision::{self, DecisionClient, DecisionError, QuestionDraft};
use aegis_lib::agent::event::EventSink;
use aegis_lib::agent::provider::fake::{WRITE_TARGET, WRITE_TRIGGER};
use aegis_lib::agent::turn::{self, TurnPlan};
use aegis_lib::audit::{AuditDecision, AuditLog};
use aegis_lib::policy::{decide, tool, ApprovalDetail, Risk};
use aegis_lib::store::DecisionSettings;
use aegis_lib::tools::{self, NullProgress, ToolCtx, ToolOutcome};
use aegis_lib::{
    Agent, ApiKey, ApprovalDecision, ApprovalRegistry, ApprovalRequest, Decision, Event,
    FakeProvider, Grant, GrantStore, HandoffCtx, MemoryStore, Message, PolicyCtx, SessionState,
    SessionStore, SkillCtx, Standing, Turn, TurnRegistry, DEFAULT_AGENT_ID,
};

// ---------------------------------------------------------------------------
// A loopback TypeSafe
// ---------------------------------------------------------------------------

/// One request as the stand-in received it.
#[derive(Debug, Clone)]
struct Seen {
    path: String,
    authorization: String,
    body: Value,
}

/// Answers each request with the next `(status, body)`; the last one repeats.
/// A status of 0 accepts the request and never answers.
struct Stub {
    base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Stub {
    async fn start(replies: Vec<(u16, Value)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);

        tokio::spawn(async move {
            let mut index = 0usize;
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let (status, body) = replies[index.min(replies.len() - 1)].clone();
                index += 1;
                let log = Arc::clone(&log);
                tokio::spawn(async move {
                    let Some(request) = read_request(&mut socket).await else {
                        return;
                    };
                    log.lock().expect("log").push(request);
                    if status == 0 {
                        tokio::time::sleep(Duration::from_secs(60)).await;
                        return;
                    }
                    let text = body.to_string();
                    let response = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
                        text.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });

        Self { base, seen }
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().expect("log").clone()
    }

    fn client(&self, annotate: bool) -> DecisionClient {
        let settings = DecisionSettings {
            model: String::new(),
            base_url: self.base.clone(),
            annotate_approvals: annotate,
        };
        DecisionClient::new(
            reqwest::Client::builder().build().ok(),
            ApiKey::new("ts-test-key"),
            &settings,
        )
        .expect("a client")
    }
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> Option<Seen> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(at) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
    let header = |name: &str| {
        head.lines()
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case(name)
                    .then(|| value.trim().to_owned())
            })
            .unwrap_or_default()
    };
    let length: usize = header("content-length").parse().unwrap_or(0);
    while buffer.len() < head_end + length {
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let path = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_default()
        .to_owned();
    Some(Seen {
        path,
        authorization: header("authorization"),
        body: serde_json::from_slice(&buffer[head_end..]).unwrap_or(Value::Null),
    })
}

fn questions(value: Value) -> Vec<decision::Question> {
    let drafts: Vec<QuestionDraft> = serde_json::from_value(value).expect("drafts");
    decision::parse_questions(drafts).expect("questions")
}

fn noul_answers(pairs: &[(&str, f64)]) -> Value {
    let answers: serde_json::Map<String, Value> = pairs
        .iter()
        .map(|(id, p)| ((*id).to_owned(), json!({ "type": "noul", "noul": p })))
        .collect();
    json!({ "model": "jev-test", "answers": answers, "usage": { "input_tokens": 1, "output_tokens": 1 } })
}

// ---------------------------------------------------------------------------
// The client
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_request_carries_the_bearer_the_path_and_a_question_map() {
    let stub = Stub::start(vec![(200, noul_answers(&[("urgent", 0.83)]))]).await;
    let client = stub.client(true);
    let asked = questions(json!([
        { "id": "urgent", "type": "noul", "instructions": "Is `ticket.text` urgent?" }
    ]));
    let state = json!({ "ticket": { "text": "the site is down", "tags": ["prod"] } });

    let answers = client
        .evaluate(&state, &asked, &CancellationToken::new())
        .await
        .expect("answered");
    assert_eq!(answers.noul("urgent"), Some(0.83));

    let seen = stub.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].path, "/v1/systemone");
    assert_eq!(seen[0].authorization, "Bearer ts-test-key");
    assert_eq!(seen[0].body["model"], "jev-latest");
    assert_eq!(seen[0].body["questions"]["urgent"]["type"], "noul");
    assert!(seen[0].body["questions"].is_object(), "a map, not an array");
    assert_eq!(
        seen[0].body["state"]["ticket"]["tags"][0], "prod",
        "structured state is sent as JSON, not a string"
    );
}

#[tokio::test]
async fn a_401_and_a_422_are_not_retried() {
    for status in [401u16, 422] {
        let stub = Stub::start(vec![(status, json!({ "error": "no" }))]).await;
        let err = stub
            .client(true)
            .evaluate(
                &json!({ "a": 1 }),
                &questions(json!([{ "id": "q", "type": "noul", "instructions": "Is it?" }])),
                &CancellationToken::new(),
            )
            .await
            .expect_err("refused");
        assert!(
            matches!(err, DecisionError::Status { status: s, .. } if s == status),
            "{err:?}"
        );
        assert_eq!(stub.seen().len(), 1, "{status} was retried");
    }
}

#[tokio::test]
async fn a_429_is_retried_then_answered() {
    let stub = Stub::start(vec![(429, json!({})), (200, noul_answers(&[("q", 0.1)]))]).await;
    let answers = stub
        .client(true)
        .evaluate(
            &json!({ "a": 1 }),
            &questions(json!([{ "id": "q", "type": "noul", "instructions": "Is it?" }])),
            &CancellationToken::new(),
        )
        .await
        .expect("answered after a retry");
    assert_eq!(answers.noul("q"), Some(0.1));
    assert_eq!(stub.seen().len(), 2);
}

#[tokio::test]
async fn a_cancel_stops_a_request_that_is_not_answering() {
    let stub = Stub::start(vec![(0, Value::Null)]).await;
    let client = stub.client(true);
    let cancel = CancellationToken::new();
    let stopper = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        stopper.cancel();
    });

    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        client.evaluate(
            &json!({ "a": 1 }),
            &questions(json!([{ "id": "q", "type": "noul", "instructions": "Is it?" }])),
            &cancel,
        ),
    )
    .await
    .expect("the cancel was honoured");
    assert_eq!(outcome.expect_err("cancelled"), DecisionError::Cancelled);
}

#[tokio::test]
async fn the_probe_tells_a_bad_key_apart() {
    let stub = Stub::start(vec![(401, json!({ "error": "bad key" }))]).await;
    let probe = stub.client(true).probe().await;
    assert!(!probe.ok);
    assert_eq!(probe.status, Some(401));
    assert!(
        probe.message.contains("rejected the key"),
        "{}",
        probe.message
    );

    let fine = Stub::start(vec![(200, noul_answers(&[("probe", 0.99)]))]).await;
    let probe = fine.client(true).probe().await;
    assert!(probe.ok, "{}", probe.message);
}

// ---------------------------------------------------------------------------
// `tool_risk` on a real turn
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct Recorder {
    events: Mutex<Vec<Event>>,
}

impl Recorder {
    fn names(&self) -> Vec<&'static str> {
        self.events
            .lock()
            .expect("events")
            .iter()
            .map(Event::name)
            .collect()
    }
}

impl EventSink for Recorder {
    fn emit(&self, event: Event) {
        self.events.lock().expect("events").push(event);
    }
}

struct App {
    _dir: TempDir,
    workspace: PathBuf,
    sessions: SessionStore,
    turns: TurnRegistry,
    grants: GrantStore,
    approvals: ApprovalRegistry,
    audit: AuditLog,
    memories: MemoryStore,
    library: PathBuf,
    session_id: String,
    agent: Agent,
}

impl App {
    fn new() -> Self {
        let dir = TempDir::new().expect("temp dir");
        let data = dir.path().join("data");
        let workspace = dir.path().join("work");
        fs::create_dir_all(&data).expect("data");
        fs::create_dir_all(&workspace).expect("work");
        let sessions = SessionStore::load(&data);
        let session_id = sessions
            .create("project-1", None, DEFAULT_AGENT_ID)
            .expect("session")
            .id;
        Self {
            workspace: dunce::canonicalize(&workspace).expect("canonical"),
            sessions,
            turns: TurnRegistry::new(),
            grants: GrantStore::new(),
            approvals: ApprovalRegistry::new(),
            audit: AuditLog::new(&data),
            memories: MemoryStore::load(&data),
            library: data.join("skills"),
            session_id,
            agent: Agent::builtin(),
            _dir: dir,
        }
    }

    /// One turn that asks to write a file, answered by `answer` once `ready`
    /// accepts the pending request.
    async fn write_turn(
        &self,
        client: Option<&DecisionClient>,
        sink: &Recorder,
        ready: impl Fn(&ApprovalRequest) -> bool,
        answer: ApprovalDecision,
    ) -> ApprovalRequest {
        self.sessions
            .append(
                &self.session_id,
                Message::user(format!("{WRITE_TRIGGER} a file for me")),
                SessionState::Running,
            )
            .expect("stored");
        let cancel = self.turns.begin(&self.session_id, "t1").expect("free");
        let plan = TurnPlan {
            session_id: self.session_id.clone(),
            turn_id: "t1".to_owned(),
            workspace: Some(self.workspace.clone()),
            exec_host: None,
        };
        let provider = FakeProvider::instant();
        let runtime = Turn {
            agent: &self.agent,
            sessions: &self.sessions,
            turns: &self.turns,
            grants: &self.grants,
            approvals: &self.approvals,
            audit: &self.audit,
            provider: &provider,
            sink,
            self_exe: None,
            captures: &self.library,
            skills: &self.library,
            memories: &self.memories,
            connectors: aegis_lib::Connectors::none(),
            standing: Standing::Own(None),
            unattended: None,
            decision: client,
        };
        let running = runtime.run(&plan, &cancel);

        let answering = async {
            for _ in 0..5000 {
                if let Some(request) = self
                    .approvals
                    .list(Some(&self.session_id))
                    .into_iter()
                    .next()
                    .filter(|request| ready(request))
                {
                    self.approvals
                        .resolve(&request.request_id, answer, &self.grants)
                        .expect("open");
                    return request;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            panic!("no approval became ready");
        };
        let (reason, request) = tokio::join!(running, answering);
        self.turns
            .finish(&self.session_id, "t1", turn::resting_state(reason));
        request
    }
}

#[tokio::test]
async fn a_likely_destructive_write_is_annotated_and_still_asked() {
    let stub = Stub::start(vec![(
        200,
        json!({
            "model": "jev-test",
            "answers": {
                "destructive": { "type": "noul", "noul": 0.95 },
                "exfil": { "type": "noul", "noul": 0.02 },
                "git_history": { "type": "noul", "noul": 0.01 },
                "bucket": { "type": "choice", "choice": "workspace_write", "probabilities": {}, "confidence": 0.3 },
                "undo": { "type": "score", "score": 0.4, "confidence": 0.9 }
            }
        }),
    )])
    .await;
    let client = stub.client(true);
    let app = App::new();
    let sink = Recorder::default();

    let request = app
        .write_turn(
            Some(&client),
            &sink,
            |request| request.annotation.is_some(),
            ApprovalDecision::Deny,
        )
        .await;

    let annotation = request.annotation.expect("annotated");
    assert!(annotation.raised);
    assert_eq!(annotation.destructive, 95);
    assert_eq!(
        annotation.bucket, None,
        "a low-confidence bucket is omitted"
    );
    // Advisory: the offer and the badge are the table's, and the deny held.
    assert!(request.session_grant_allowed);
    assert_eq!(request.risk, Risk::Medium);
    assert!(!app.workspace.join(WRITE_TARGET).exists());
    assert!(sink.names().contains(&"tool:approval_annotated"));

    let seen = stub.seen();
    assert_eq!(seen[0].body["state"]["tool"], "fs_write");
    assert!(seen[0].body["state"].get("transcript").is_none());
}

#[tokio::test]
async fn a_failing_decision_model_leaves_the_ask_as_it_was() {
    let stub = Stub::start(vec![(500, json!({ "error": "down" }))]).await;
    let client = stub.client(true);
    let app = App::new();
    let sink = Recorder::default();

    // Wait until the stand-in has been asked, so the failure has happened.
    let request = app
        .write_turn(
            Some(&client),
            &sink,
            |_| !stub.seen().is_empty(),
            ApprovalDecision::AllowOnce,
        )
        .await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_eq!(request.annotation, None);
    assert!(!sink.names().contains(&"tool:approval_annotated"));
    assert!(
        app.workspace.join(WRITE_TARGET).exists(),
        "the ask still ran"
    );
}

#[tokio::test]
async fn the_toggle_off_sends_nothing() {
    let stub = Stub::start(vec![(200, noul_answers(&[]))]).await;
    let client = stub.client(false);
    let app = App::new();
    let sink = Recorder::default();

    app.write_turn(Some(&client), &sink, |_| true, ApprovalDecision::Deny)
        .await;
    assert!(stub.seen().is_empty(), "annotation was off");
}

// ---------------------------------------------------------------------------
// `jev_ask` and `jev_eval` through policy and dispatch
// ---------------------------------------------------------------------------

struct Tools {
    _dir: TempDir,
    workspace: PathBuf,
    grants: GrantStore,
    audit: AuditLog,
    memories: MemoryStore,
    data: PathBuf,
}

impl Tools {
    fn new() -> Self {
        let dir = TempDir::new().expect("temp dir");
        let workspace = dir.path().join("work");
        let data = dir.path().join("data");
        fs::create_dir_all(&workspace).expect("work");
        fs::create_dir_all(&data).expect("data");
        Self {
            workspace: dunce::canonicalize(&workspace).expect("canonical"),
            grants: GrantStore::new(),
            audit: AuditLog::new(&data),
            memories: MemoryStore::load(&data),
            data,
            _dir: dir,
        }
    }

    fn put(&self, relative: &str, text: &str) {
        let path = self.workspace.join(relative);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, text).expect("write");
    }

    fn policy(&self) -> PolicyCtx<'_> {
        PolicyCtx::new("s1", Some(&self.workspace), &self.grants)
    }

    async fn run(
        &self,
        client: Option<&DecisionClient>,
        decision: Decision,
        args: &Value,
    ) -> ToolOutcome {
        let cancel = CancellationToken::new();
        let ctx = ToolCtx {
            session_id: "s1",
            agent_id: DEFAULT_AGENT_ID,
            turn_id: "t1",
            call_id: "c1",
            audit: &self.audit,
            captures: &self.data,
            args,
            progress: &NullProgress,
            cancel: &cancel,
            skills: SkillCtx {
                library: &self.data,
                workspace: None,
                tools: &[],
                active: None,
            },
            memories: &self.memories,
            handoffs: HandoffCtx {
                bus: None,
                open: None,
            },
            connectors: aegis_lib::Connectors::none(),
            routine: "",
            decision: client,
        };
        match decision {
            Decision::Ask { call, request } => {
                tools::run(&ctx, AuditDecision::AllowOnce, &request.reason, &call).await
            }
            other => panic!("expected an ask, got {other:?}"),
        }
    }
}

fn ask_args() -> Value {
    json!({
        "state": { "mail": { "subject": "Invoice 42", "body": "Please pay by Friday" } },
        "questions": [
            { "id": "is_invoice", "type": "noul", "instructions": "Is `mail` an invoice?" },
            { "id": "tone", "type": "choice", "instructions": "Tone of `mail.body`?",
              "options": { "polite": "courteous", "curt": "short or demanding" } }
        ]
    })
}

#[tokio::test]
async fn jev_ask_always_asks_offers_its_grant_and_is_high_risk() {
    let fx = Tools::new();
    match decide(&fx.policy(), tool::JEV_ASK, ask_args()) {
        Decision::Ask { request, .. } => {
            assert_eq!(request.risk, Risk::High);
            assert_eq!(request.grant, Some(Grant::JevAsk));
            match &request.detail {
                ApprovalDetail::JevAsk {
                    question_count,
                    questions,
                    state_preview,
                    ..
                } => {
                    assert_eq!(*question_count, 2);
                    assert!(
                        questions[0].starts_with("is_invoice (noul)"),
                        "{questions:?}"
                    );
                    assert!(state_preview.contains("Invoice 42"));
                }
                other => panic!("{other:?}"),
            }
        }
        other => panic!("expected an ask, got {other:?}"),
    }

    fx.grants.insert("s1", Grant::JevAsk);
    assert!(matches!(
        decide(&fx.policy(), tool::JEV_ASK, ask_args()),
        Decision::Auto { .. }
    ));

    let fresh = GrantStore::new();
    let unattended = PolicyCtx::new("s1", Some(&fx.workspace), &fresh).unattended(true);
    assert!(matches!(
        decide(&unattended, tool::JEV_ASK, ask_args()),
        Decision::Deny { .. }
    ));
}

#[test]
fn jev_ask_refuses_a_malformed_call_at_parse() {
    let fx = Tools::new();
    let cases = [
        json!({ "state": {}, "questions": [{ "id": "a", "type": "noul", "instructions": "x" }] }),
        json!({ "state": { "a": 1 }, "questions": [
            { "id": "a", "type": "noul", "instructions": "x" },
            { "id": "a", "type": "noul", "instructions": "y" }
        ] }),
        json!({ "state": { "a": 1 }, "questions": [{ "id": "a", "type": "choice", "instructions": "x" }] }),
        json!({ "state": { "a": 1 }, "questions": [] }),
    ];
    for args in cases {
        match decide(&fx.policy(), tool::JEV_ASK, args.clone()) {
            Decision::Deny { code, .. } => assert_eq!(code.as_str(), "E_TOOL_FAILED"),
            other => panic!("{args} gave {other:?}"),
        }
    }
}

#[tokio::test]
async fn jev_ask_without_a_key_is_an_envelope_after_approval() {
    let fx = Tools::new();
    let args = ask_args();
    let outcome = fx
        .run(
            None,
            decide(&fx.policy(), tool::JEV_ASK, args.clone()),
            &args,
        )
        .await;
    assert!(!outcome.result.ok);
    assert_eq!(
        outcome.result.error.as_ref().map(|e| e.code.as_str()),
        Some("E_NO_API_KEY")
    );
}

#[tokio::test]
async fn jev_ask_returns_the_answers() {
    let stub = Stub::start(vec![(
        200,
        json!({ "model": "jev-test", "answers": {
            "is_invoice": { "type": "noul", "noul": 0.97 },
            "tone": { "type": "choice", "choice": "curt", "probabilities": { "curt": 0.8 }, "confidence": 0.8 }
        }}),
    )])
    .await;
    let client = stub.client(true);
    let fx = Tools::new();
    let args = ask_args();
    let outcome = fx
        .run(
            Some(&client),
            decide(&fx.policy(), tool::JEV_ASK, args.clone()),
            &args,
        )
        .await;
    assert!(outcome.result.ok, "{:?}", outcome.result.error);
    let content: Value = serde_json::from_str(&outcome.result.content).expect("json");
    assert_eq!(content["answers"]["tone"]["choice"], "curt");
    assert_eq!(
        stub.seen()[0].body["questions"]["tone"]["criteria"]["curt"],
        "short or demanding"
    );
}

const EVAL: &str = r#"name: inbox.classify
when: Classify the newest inbound mail
inputs:
  mail: inbox/latest.md
questions:
  is_invoice:
    type: noul
    instructions: Does `mail` contain an invoice?
compose:
  - when: noul is_invoice >= 0.8
    route: billing
  - when: noul is_invoice < 0.2
    escalate: needs_you
    say: This mail is not what the inbox expects.
"#;

#[tokio::test]
async fn a_signed_eval_runs_and_returns_a_route_not_an_action() {
    let stub = Stub::start(vec![(200, noul_answers(&[("is_invoice", 0.9)]))]).await;
    let client = stub.client(true);
    let fx = Tools::new();
    fx.put(".aegis/evals/inbox.classify/eval.yml", EVAL);
    fx.put("inbox/latest.md", "Invoice 42: 300 EUR");
    fx.put("inbox/other.md", "Hello there");

    let args = json!({ "name": "inbox.classify" });
    let decision = decide(&fx.policy(), tool::JEV_EVAL, args.clone());
    match &decision {
        Decision::Ask { request, .. } => {
            assert_eq!(request.risk, Risk::High);
            assert_eq!(
                request.grant,
                Some(Grant::JevEval {
                    name: "inbox.classify".to_owned()
                })
            );
            match &request.detail {
                ApprovalDetail::JevEval {
                    inputs, questions, ..
                } => {
                    assert_eq!(questions, &["is_invoice"]);
                    assert!(inputs[0].starts_with("mail: "), "{inputs:?}");
                }
                other => panic!("{other:?}"),
            }
        }
        other => panic!("expected an ask, got {other:?}"),
    }

    let outcome = fx.run(Some(&client), decision, &args).await;
    assert!(outcome.result.ok, "{:?}", outcome.result.error);
    let content: Value = serde_json::from_str(&outcome.result.content).expect("json");
    assert_eq!(content["routes"], json!(["billing"]));
    assert_eq!(content["escalations"], json!([]));
    assert_eq!(
        stub.seen()[0].body["state"]["mail"],
        "Invoice 42: 300 EUR",
        "the harness read the declared input"
    );

    // An override replaces a declared input; it cannot add one.
    let args = json!({ "name": "inbox.classify", "inputs": { "mail": "inbox/other.md" } });
    fx.run(
        Some(&client),
        decide(&fx.policy(), tool::JEV_EVAL, args.clone()),
        &args,
    )
    .await;
    assert_eq!(stub.seen()[1].body["state"]["mail"], "Hello there");
    for args in [
        json!({ "name": "inbox.classify", "inputs": { "extra": "inbox/other.md" } }),
        json!({ "name": "inbox.classify", "inputs": { "mail": "../outside.md" } }),
    ] {
        assert!(
            matches!(
                decide(&fx.policy(), tool::JEV_EVAL, args.clone()),
                Decision::Deny { .. }
            ),
            "{args}"
        );
    }
}

#[test]
fn a_proposal_that_was_not_applied_cannot_run() {
    let fx = Tools::new();
    fx.put(".aegis/evals/inbox.classify/PROPOSAL.yml", EVAL);
    match decide(
        &fx.policy(),
        tool::JEV_EVAL,
        json!({ "name": "inbox.classify" }),
    ) {
        Decision::Deny { reason, .. } => assert!(reason.contains("only proposed"), "{reason}"),
        other => panic!("expected a denial, got {other:?}"),
    }
}

#[test]
fn applying_an_eval_proposal_is_asked_every_time_and_never_from_a_brief() {
    let fx = Tools::new();
    fx.put(".aegis/evals/inbox.classify/PROPOSAL.yml", EVAL);
    let apply = json!({
        "path": ".aegis/evals/inbox.classify/eval.yml",
        "content": EVAL,
        "create_dirs": true
    });

    fx.grants.insert("s1", Grant::FsWrite);
    match decide(&fx.policy(), tool::FS_WRITE, apply.clone()) {
        Decision::Ask { request, .. } => {
            assert_eq!(request.title, "Apply an eval proposal");
            assert_eq!(request.grant, None, "a held FsWrite does not sign an eval");
            assert_eq!(request.risk, Risk::High);
        }
        other => panic!("expected an ask, got {other:?}"),
    }

    let direct = json!({
        "path": ".aegis/evals/inbox.classify/eval.yml",
        "content": "name: inbox.classify\n",
    });
    match decide(&fx.policy(), tool::FS_WRITE, direct) {
        Decision::Ask { request, .. } => {
            assert_eq!(request.title, "Write a project eval");
            assert_eq!(request.grant, None);
        }
        other => panic!("expected an ask, got {other:?}"),
    }

    assert!(matches!(
        decide(&fx.policy().delegated(), tool::FS_WRITE, apply.clone()),
        Decision::Deny { .. }
    ));

    // Once applied, a second apply would replace it, which is refused.
    fx.put(".aegis/evals/inbox.classify/eval.yml", "name: old\n");
    assert!(matches!(
        decide(&fx.policy(), tool::FS_WRITE, apply),
        Decision::Deny { .. }
    ));
}
