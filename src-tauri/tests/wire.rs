//! The wire, end to end: a real socket, a real HTTP response, real SSE framing
//! (PLAN 6, Phase 8 — `tests/wire.rs`: "SSE parse + tool-call assembly").
//!
//! Covers what needs a socket: the request body (PLAN 4.1), the key only in a
//! header, payloads split across writes, and cancel closing the connection. The
//! last test is Phase 8's exit criterion: a full tool round trip. The server is
//! raw `tokio::net`, so it cannot share the client's HTTP assumptions.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use aegis_lib::agent::event::EventSink;
use aegis_lib::agent::provider::openai;
use aegis_lib::agent::turn::TurnPlan;
use aegis_lib::agent::wire::{ToolCallAssembler, WireMessage};
use aegis_lib::Standing;
use aegis_lib::{
    Agent, ApiKey, ApprovalRegistry, AuditDecision, AuditLog, Event, GrantStore, MemoryStore,
    Message, ModelEvent, ModelRequest, OpenAiProvider, Provider, ProviderSettings, Role,
    SessionState, SessionStore, StopReason, Turn, TurnRegistry, DEFAULT_AGENT_ID,
};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};

/// How long a test waits on the fixture before calling it a hang.
const PATIENCE: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// A scripted HTTP server
// ---------------------------------------------------------------------------

/// What the client sent, as the server saw it.
#[derive(Debug)]
struct Recorded {
    request_line: String,
    /// Header names lowercased; values as they arrived.
    headers: Vec<(String, String)>,
    body: String,
}

impl Recorded {
    /// The first value of a header, matched case-insensitively.
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(found, _)| found == name)
            .map(|(_, value)| value.as_str())
    }

    /// The request body as JSON.
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).expect("the request body is JSON")
    }
}

/// How the fixture answers.
enum Answer {
    /// A `200 text/event-stream`, written as these byte groups in order.
    ///
    /// One group is one `write` — which is what lets a test put a chunk
    /// boundary in the middle of a payload and prove the decoder survives it.
    Stream(Vec<Vec<u8>>),
    /// A `200 application/json`, for the probe's non-streamed completion.
    Json(String),
    /// A failure status with a JSON body.
    Status(u16, String),
    /// Headers and one frame, then silence: the connection is held open until
    /// the client closes it.
    Hang(oneshot::Sender<()>),
}

/// A server that answers a fixed list of requests, one connection each.
///
/// `Connection: close` on each marks the end of the body without chunking.
struct Server {
    base_url: String,
    recorded: mpsc::Receiver<Recorded>,
}

impl Server {
    /// Starts the fixture on a loopback port the OS picks.
    async fn start(answers: Vec<Answer>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
        let port = listener.local_addr().expect("an address").port();
        let (tx, recorded) = mpsc::channel(answers.len().max(1));

        tokio::spawn(async move {
            for answer in answers {
                let (mut socket, _) = listener.accept().await.expect("a connection");
                let request = read_request(&mut socket).await;
                let _ = tx.send(request).await;
                write_answer(&mut socket, answer).await;
            }
        });

        Self {
            // No `/v1`: the base URL is whatever the user configured, and the
            // provider appends the endpoint path to it.
            base_url: format!("http://127.0.0.1:{port}"),
            recorded,
        }
    }

    /// The next request the client sent, once it has sent it.
    async fn recorded(&mut self) -> Recorded {
        tokio::time::timeout(PATIENCE, self.recorded.recv())
            .await
            .expect("the client sent a request")
            .expect("the fixture recorded it")
    }
}

/// Reads one HTTP request: head, then exactly `Content-Length` bytes.
async fn read_request(socket: &mut TcpStream) -> Recorded {
    let mut raw = Vec::new();
    let mut byte = [0_u8; 1];

    // Byte at a time: the head has to stop exactly at the blank line, because
    // whatever follows it is the body and belongs to the next read.
    while !raw.ends_with(b"\r\n\r\n") {
        let read = socket.read(&mut byte).await.expect("the request head");
        assert_eq!(read, 1, "the client closed before finishing its request");
        raw.push(byte[0]);
    }

    let head = String::from_utf8_lossy(&raw).into_owned();
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default().to_owned();

    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_lowercase(), value.trim().to_owned()))
        .collect();

    let length: usize = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);

    let mut body = vec![0_u8; length];
    socket
        .read_exact(&mut body)
        .await
        .expect("the request body");

    Recorded {
        request_line,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}

/// Writes the fixture's answer and closes, which is what ends the body.
async fn write_answer(socket: &mut TcpStream, answer: Answer) {
    match answer {
        Answer::Stream(groups) => {
            let head = "HTTP/1.1 200 OK\r\n\
                        Content-Type: text/event-stream\r\n\
                        Cache-Control: no-cache\r\n\
                        Connection: close\r\n\r\n";
            socket.write_all(head.as_bytes()).await.expect("the head");

            for group in groups {
                if socket.write_all(&group).await.is_err() {
                    // The client hung up mid-reply. That is a pass, not a
                    // failure: it is what cancellation looks like from here.
                    return;
                }
                socket.flush().await.expect("a flush");
            }
        }
        Answer::Json(body) => {
            let head = format!(
                "HTTP/1.1 200 OK\r\n\
                 Content-Type: application/json\r\n\
                 Content-Length: {}\r\n\
                 Connection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(head.as_bytes()).await.expect("the head");
            socket.write_all(body.as_bytes()).await.expect("the body");
        }
        Answer::Status(status, body) => {
            let head = format!(
                "HTTP/1.1 {status} Error\r\n\
                 Content-Type: application/json\r\n\
                 Content-Length: {}\r\n\
                 Connection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(head.as_bytes()).await.expect("the head");
            socket.write_all(body.as_bytes()).await.expect("the body");
        }
        Answer::Hang(closed) => {
            let head = "HTTP/1.1 200 OK\r\n\
                        Content-Type: text/event-stream\r\n\
                        Connection: close\r\n\r\n";
            socket.write_all(head.as_bytes()).await.expect("the head");
            socket
                .write_all(
                    frame(&json!({ "choices": [{ "delta": { "content": "…" } }] })).as_slice(),
                )
                .await
                .expect("one frame");
            socket.flush().await.expect("a flush");

            // Then wait. A read of zero bytes is the client closing the
            // connection, which is the thing under test.
            let mut sink = [0_u8; 64];
            let read = socket.read(&mut sink).await.unwrap_or(0);
            assert_eq!(read, 0, "the client sent something instead of hanging up");
            let _ = closed.send(());
        }
    }
    let _ = socket.shutdown().await;
}

/// One SSE frame carrying `value`.
fn frame(value: &Value) -> Vec<u8> {
    format!("data: {value}\n\n").into_bytes()
}

/// The `[DONE]` sentinel that ends a stream.
fn done() -> Vec<u8> {
    b"data: [DONE]\n\n".to_vec()
}

// ---------------------------------------------------------------------------
// Driving the provider
// ---------------------------------------------------------------------------

/// The settings and key a test streams with.
fn provider(base_url: &str) -> OpenAiProvider {
    OpenAiProvider::new(
        openai::client(),
        &ProviderSettings {
            auth_kind: aegis_lib::AuthKind::ApiKey,
            base_url: base_url.to_owned(),
            model: "test-model".to_owned(),
            max_output_tokens: None,
        },
        ApiKey::new("sk-test-key-abcd1234"),
    )
}

/// A request with one user message and one tool on offer.
fn request() -> ModelRequest {
    ModelRequest {
        model: "test-model".to_owned(),
        messages: vec![
            WireMessage::System {
                content: "rules".to_owned(),
            },
            WireMessage::User {
                content: "hello".to_owned(),
            },
        ],
        tools: vec![json!({
            "type": "function",
            "function": { "name": "fs_list", "parameters": { "type": "object" } }
        })],
    }
}

/// Everything a stream produced, or a failure if it took too long.
async fn drain(mut stream: mpsc::Receiver<ModelEvent>) -> Vec<ModelEvent> {
    let collect = async {
        let mut events = Vec::new();
        while let Some(event) = stream.recv().await {
            events.push(event);
        }
        events
    };

    tokio::time::timeout(PATIENCE, collect)
        .await
        .expect("the stream ended")
}

/// The text of every delta, concatenated.
fn text_of(events: &[ModelEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            ModelEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The request Aegis sends
// ---------------------------------------------------------------------------

/// PLAN 4.1: the endpoint is the base URL plus `/chat/completions`, the body
/// streams, and the tools travel with it.
#[tokio::test]
async fn the_request_is_the_documented_shape() {
    let mut server = Server::start(vec![Answer::Stream(vec![
        frame(&json!({ "choices": [{ "delta": { "content": "hi" }, "finish_reason": "stop" }] })),
        done(),
    ])])
    .await;

    let events = drain(provider(&server.base_url).stream(request())).await;
    assert_eq!(text_of(&events), "hi");

    let sent = server.recorded().await;
    assert_eq!(
        sent.request_line, "POST /chat/completions HTTP/1.1",
        "the endpoint is the base URL with the path appended"
    );
    assert_eq!(sent.header("content-type"), Some("application/json"));
    assert_eq!(sent.header("accept"), Some("text/event-stream"));

    let body = sent.json();
    assert_eq!(body["model"], "test-model");
    assert_eq!(body["stream"], true);
    assert_eq!(
        body["stream_options"]["include_usage"], true,
        "usage has to be asked for on the wire, or every turn comes back \
         unmeasured and the board has nothing to count"
    );
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][1]["content"], "hello");
    assert_eq!(body["tools"][0]["function"]["name"], "fs_list");
    assert_eq!(body["tool_choice"], "auto");
}

/// The key travels in the `Authorization` header and nowhere else. A key that
/// reached the body would be logged by every proxy on the way.
#[tokio::test]
async fn the_key_travels_as_a_bearer_header_and_never_in_the_body() {
    let mut server = Server::start(vec![Answer::Stream(vec![done()])]).await;

    drain(provider(&server.base_url).stream(request())).await;

    let sent = server.recorded().await;
    assert_eq!(
        sent.header("authorization"),
        Some("Bearer sk-test-key-abcd1234")
    );
    assert!(
        !sent.body.contains("sk-test-key"),
        "the key must not appear in the request body: {}",
        sent.body
    );
}

// ---------------------------------------------------------------------------
// What comes back
// ---------------------------------------------------------------------------

/// The whole point of the decoder: the network splits the stream wherever it
/// likes, and the payloads still arrive whole. Every byte is its own write
/// here, which is the worst boundary the transport could pick.
#[tokio::test]
async fn a_reply_split_at_every_byte_still_assembles() {
    let mut whole = Vec::new();
    whole.extend(frame(
        &json!({ "choices": [{ "delta": { "content": "Bonjour, " } }] }),
    ));
    whole.extend(frame(
        &json!({ "choices": [{ "delta": { "content": "ça va ?" } }] }),
    ));
    whole.extend(frame(
        &json!({ "choices": [{ "delta": {}, "finish_reason": "stop" }] }),
    ));
    whole.extend(done());

    let server = Server::start(vec![Answer::Stream(
        whole.into_iter().map(|byte| vec![byte]).collect(),
    )])
    .await;

    let events = drain(provider(&server.base_url).stream(request())).await;

    assert_eq!(
        text_of(&events),
        "Bonjour, ça va ?",
        "including the multi-byte characters cut in half by a write boundary"
    );
    assert!(matches!(
        events.last(),
        Some(ModelEvent::Finish {
            reason: aegis_lib::StopReason::Stop,
            ..
        })
    ));
}

/// SSE parse plus tool-call assembly, which is what this file is named for:
/// fragments arrive across frames, and what comes out the far end is one
/// parsed call the turn loop could hand to policy.
#[tokio::test]
async fn streamed_tool_call_fragments_assemble_into_one_parsed_call() {
    let server = Server::start(vec![Answer::Stream(vec![
        frame(&json!({ "choices": [{ "delta": { "tool_calls": [{
            "index": 0, "id": "call_x", "type": "function",
            "function": { "name": "fs_list", "arguments": "" }
        }] } }] })),
        frame(&json!({ "choices": [{ "delta": { "tool_calls": [{
            "index": 0, "function": { "arguments": "{\"pa" }
        }] } }] })),
        frame(&json!({ "choices": [{ "delta": { "tool_calls": [{
            "index": 0, "function": { "arguments": "th\":\".\"}" }
        }] } }] })),
        frame(&json!({ "choices": [{ "delta": {}, "finish_reason": "tool_calls" }] })),
        done(),
    ])])
    .await;

    let events = drain(provider(&server.base_url).stream(request())).await;

    let mut assembler = ToolCallAssembler::default();
    for event in &events {
        if let ModelEvent::ToolCallDelta {
            index,
            id,
            name,
            args_delta,
            ..
        } = event
        {
            assembler.push(*index, id.clone(), name.clone(), args_delta);
        }
    }

    let calls = assembler.finish();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].call_id, "call_x");
    assert_eq!(calls[0].name, "fs_list");
    assert_eq!(
        calls[0].args.as_ref().expect("valid arguments")["path"],
        json!(".")
    );

    assert!(matches!(
        events.last(),
        Some(ModelEvent::Finish {
            reason: aegis_lib::StopReason::ToolCalls,
            ..
        })
    ));
}

/// Usage arriving in a chunk of its own, after the one that said why the model
/// stopped. The turn loop stops reading at the first finish it is handed, so
/// the provider holds the finish back until the stream is over.
#[tokio::test]
async fn usage_reported_after_the_finish_reason_still_arrives() {
    let server = Server::start(vec![Answer::Stream(vec![
        frame(&json!({ "choices": [{ "delta": { "content": "ok" }, "finish_reason": "stop" }] })),
        frame(&json!({
            "choices": [],
            "usage": { "prompt_tokens": 9, "completion_tokens": 1, "total_tokens": 10 }
        })),
        done(),
    ])])
    .await;

    let events = drain(provider(&server.base_url).stream(request())).await;

    let Some(ModelEvent::Finish {
        usage: Some(usage), ..
    }) = events.last()
    else {
        panic!("expected a finish carrying usage, got {events:?}");
    };
    assert_eq!(usage.total_tokens, 10);
}

/// A failure status becomes one error event carrying the server's own words,
/// and nothing else. `E_PROVIDER_HTTP` on a 401 is not retryable: the same
/// request will be rejected in exactly the same way.
#[tokio::test]
async fn a_rejected_key_becomes_one_error_event_quoting_the_server() {
    let server = Server::start(vec![Answer::Status(
        401,
        json!({ "error": { "message": "Incorrect API key provided" } }).to_string(),
    )])
    .await;

    let events = drain(provider(&server.base_url).stream(request())).await;

    assert_eq!(events.len(), 1, "{events:?}");
    let ModelEvent::Error {
        code,
        message,
        retryable,
    } = &events[0]
    else {
        panic!("expected an error, got {events:?}");
    };
    assert_eq!(code, "E_PROVIDER_HTTP");
    assert!(message.contains("Incorrect API key provided"), "{message}");
    assert!(!retryable, "a rejected key fails the same way every time");
}

/// A 500 is worth retrying, which is what the UI uses to decide whether to
/// offer the button at all.
#[tokio::test]
async fn a_server_failure_is_reported_as_retryable() {
    let server = Server::start(vec![Answer::Status(
        503,
        String::from("upstream unavailable"),
    )])
    .await;

    let events = drain(provider(&server.base_url).stream(request())).await;

    let ModelEvent::Error {
        message, retryable, ..
    } = &events[0]
    else {
        panic!("expected an error, got {events:?}");
    };
    assert!(retryable, "a 503 is worth trying again");
    assert!(message.contains("upstream unavailable"), "{message}");
}

/// A stream that stops mid-reply, with no `[DONE]` and no `finish_reason`,
/// must not be dressed up as a finished one — the turn loop reports the
/// truncation, and it can only do that if no finish arrives.
#[tokio::test]
async fn a_truncated_stream_produces_text_and_no_finish() {
    let server = Server::start(vec![Answer::Stream(vec![frame(
        &json!({ "choices": [{ "delta": { "content": "half a sen" } }] }),
    )])])
    .await;

    let events = drain(provider(&server.base_url).stream(request())).await;

    assert_eq!(text_of(&events), "half a sen");
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ModelEvent::Finish { .. })),
        "{events:?}"
    );
}

/// Cancellation. The turn loop cancels by dropping the receiver, and the
/// provider must take that as an instruction to hang up — not to read a reply
/// nobody will see, from a server still being paid to produce it.
#[tokio::test]
async fn dropping_the_receiver_closes_the_connection() {
    let (closed_tx, closed) = oneshot::channel();
    let server = Server::start(vec![Answer::Hang(closed_tx)]).await;

    let provider = Arc::new(provider(&server.base_url));
    let mut stream = provider.stream(request());

    // Wait for the first frame, so the connection is genuinely established and
    // streaming before it is abandoned.
    let first = tokio::time::timeout(PATIENCE, stream.recv())
        .await
        .expect("a first event")
        .expect("a first event");
    assert!(matches!(first, ModelEvent::TextDelta { .. }));

    drop(stream);

    tokio::time::timeout(PATIENCE, closed)
        .await
        .expect("the provider hung up rather than reading on")
        .expect("the fixture saw the close");
}

// ---------------------------------------------------------------------------
// Through the turn loop
// ---------------------------------------------------------------------------

/// A sink that keeps every event a turn emits.
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

    /// The concatenated text of every `turn:delta`.
    fn streamed(&self) -> String {
        self.events()
            .iter()
            .filter_map(|event| match event {
                Event::TurnDelta(delta) => Some(delta.text.clone()),
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

/// The exit criterion of the phase, and the claim the `Provider` trait was
/// introduced to make good on: a real HTTP provider drives the same turn loop
/// the scripted one does, with nothing in `agent/turn.rs` aware of the
/// difference.
///
/// Text, an auto-allowed `fs_list`, then text: asserts events, transcript,
/// audit line and the tool result in the second request.
#[tokio::test]
async fn a_real_provider_drives_a_whole_turn_including_a_tool_call() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let data = dir.path().join("data");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&workspace).expect("workspace dir");
    std::fs::write(workspace.join("notes.txt"), "hello").expect("a file to list");

    let mut server = Server::start(vec![
        // Round one: a sentence, then a call the model chose itself.
        Answer::Stream(vec![
            frame(&json!({ "choices": [{ "delta": { "content": "Let me look. " } }] })),
            frame(&json!({ "choices": [{ "delta": { "tool_calls": [{
                "index": 0, "id": "call_1", "type": "function",
                "function": { "name": "fs_list", "arguments": "{\"path\":\".\"}" }
            }] } }] })),
            frame(&json!({ "choices": [{ "delta": {}, "finish_reason": "tool_calls" }] })),
            done(),
        ]),
        // Round two: what it made of the result.
        Answer::Stream(vec![
            frame(&json!({ "choices": [{ "delta": { "content": "There is one file." } }] })),
            frame(&json!({ "choices": [{ "delta": {}, "finish_reason": "stop" }] })),
            done(),
        ]),
    ])
    .await;

    let sessions = SessionStore::load(&data);
    let turns = TurnRegistry::new();
    let audit = AuditLog::new(&data);
    let sink = Recorder::default();

    let session = sessions
        .create("project-1", None, DEFAULT_AGENT_ID)
        .expect("a session");
    sessions
        .append(
            &session.id,
            Message::user("what is in the workspace?"),
            SessionState::Running,
        )
        .expect("the message is recorded");

    let turn_id = "turn-1";
    let cancel = turns
        .begin(&session.id, turn_id)
        .expect("the session is free");
    let plan = TurnPlan {
        session_id: session.id.clone(),
        turn_id: turn_id.to_owned(),
        workspace: Some(dunce::canonicalize(&workspace).expect("canonical workspace")),
        exec_host: None,
    };

    let reason = Turn {
        agent: &Agent::builtin(),
        sessions: &sessions,
        turns: &turns,
        grants: &GrantStore::new(),
        approvals: &ApprovalRegistry::new(),
        audit: &audit,
        provider: &provider(&server.base_url),
        sink: &sink,
        self_exe: None,
        captures: &data.join("captures"),
        skills: &data.join("skills"),
        memories: &MemoryStore::load(&data),
        connectors: aegis_lib::Connectors::none(),
        standing: Standing::Own(None),
        unattended: None,
    }
    .run(&plan, &cancel)
    .await;

    assert_eq!(reason, StopReason::Stop);
    assert_eq!(sink.streamed(), "Let me look. There is one file.");

    // The transcript: what the user said, what the model said and asked for,
    // the answer to the call, and the reply that followed it.
    let history = sessions.messages(&session.id).expect("the transcript");
    let roles: Vec<Role> = history.iter().map(|message| message.role).collect();
    assert_eq!(
        roles,
        vec![Role::User, Role::Assistant, Role::Tool, Role::Assistant],
        "{history:#?}"
    );
    assert_eq!(history[1].tool_calls[0].tool, "fs_list");
    assert!(
        history[2].text.contains("notes.txt"),
        "the tool actually ran: {}",
        history[2].text
    );

    // One audit line, and the decision was policy's own — a read inside the
    // workspace is not something a person is asked about.
    let audited = audit.tail(10, Some(&session.id)).expect("the audit log");
    assert_eq!(audited.len(), 1, "{audited:#?}");
    assert_eq!(audited[0].tool, "fs_list");
    assert_eq!(audited[0].decision, AuditDecision::Auto);

    // And the second request carried the result back, which is the half of the
    // loop a provider test cannot see.
    let first = server.recorded().await;
    assert_eq!(first.json()["messages"].as_array().map(Vec::len), Some(2));

    let second = server.recorded().await.json();
    let messages = second["messages"].as_array().expect("messages");
    assert_eq!(messages.len(), 4, "{messages:#?}");
    assert_eq!(messages[2]["role"], "assistant");
    assert_eq!(messages[2]["tool_calls"][0]["id"], "call_1");
    assert_eq!(messages[3]["role"], "tool");
    assert_eq!(messages[3]["tool_call_id"], "call_1");
    assert!(
        messages[3]["content"]
            .as_str()
            .is_some_and(|envelope| envelope.contains("notes.txt")),
        "the model is shown what the tool returned: {}",
        messages[3]["content"]
    );
}

// ---------------------------------------------------------------------------
// The connection test
// ---------------------------------------------------------------------------

/// The settings the probe tests are run against.
fn probe_settings(base_url: &str) -> ProviderSettings {
    ProviderSettings {
        auth_kind: aegis_lib::AuthKind::ApiKey,
        base_url: base_url.to_owned(),
        model: "test-model".to_owned(),
        max_output_tokens: None,
    }
}

/// The probe exercises the endpoint a turn would use, not a models listing.
///
/// `/models` can fail on a working configuration.
#[tokio::test]
async fn the_probe_asks_the_chat_endpoint_and_names_the_model_that_answered() {
    let mut server = Server::start(vec![Answer::Json(
        json!({
            "id": "chatcmpl-1",
            "model": "test-model",
            "choices": [{ "index": 0, "message": { "role": "assistant", "content": "ok" },
                          "finish_reason": "stop" }]
        })
        .to_string(),
    )])
    .await;

    let key = ApiKey::new("sk-test-key-abcd1234");
    let probe = openai::probe(
        openai::client().as_ref(),
        &probe_settings(&server.base_url),
        key.as_ref(),
    )
    .await;

    assert!(probe.ok, "{}", probe.message);
    assert_eq!(probe.status, Some(200));
    assert!(probe.latency_ms.is_some());
    assert!(probe.message.contains("test-model"), "{}", probe.message);

    let sent = server.recorded().await;
    assert_eq!(sent.request_line, "POST /chat/completions HTTP/1.1");
    assert_eq!(
        sent.header("authorization"),
        Some("Bearer sk-test-key-abcd1234")
    );

    let body = sent.json();
    assert_eq!(body["model"], "test-model");
    assert!(
        body.get("stream").is_none_or(|stream| stream == false),
        "a probe has nothing to watch arrive: {body}"
    );
    assert!(
        body["max_tokens"].as_u64().is_some_and(|cap| cap <= 32),
        "a probe should cost a handful of tokens: {body}"
    );
}

/// A gateway that resolves an alias to something else is worth seeing before
/// it turns up on an invoice.
#[tokio::test]
async fn the_probe_says_when_the_server_answered_as_a_different_model() {
    let server = Server::start(vec![Answer::Json(
        json!({ "model": "some-vendor/test-model-0709", "choices": [] }).to_string(),
    )])
    .await;

    let probe = openai::probe(
        openai::client().as_ref(),
        &probe_settings(&server.base_url),
        ApiKey::new("sk-test-key-abcd1234").as_ref(),
    )
    .await;

    assert!(probe.ok, "{}", probe.message);
    assert!(probe.message.contains("rather than"), "{}", probe.message);
    assert!(probe.message.contains("some-vendor/test-model-0709"));
}

/// The three failures the probe exists to tell apart, each with the server's
/// own words attached.
#[tokio::test]
async fn the_probe_separates_a_bad_key_from_a_bad_address_from_a_bad_model() {
    let cases = [
        (
            401_u16,
            json!({ "error": { "message": "invalid x-api-key" } }).to_string(),
            "rejected the API key",
        ),
        (404, String::from("not found"), "no chat endpoint"),
        (
            400,
            json!({ "error": { "message": "model: unknown model" } }).to_string(),
            "not a model it serves",
        ),
    ];

    for (status, body, expected) in cases {
        let server = Server::start(vec![Answer::Status(status, body)]).await;
        let probe = openai::probe(
            openai::client().as_ref(),
            &probe_settings(&server.base_url),
            ApiKey::new("sk-test-key-abcd1234").as_ref(),
        )
        .await;

        assert!(!probe.ok, "{status} should not read as success");
        assert_eq!(probe.status, Some(status));
        assert!(
            probe.message.contains(expected),
            "{status} gave `{}`",
            probe.message
        );
    }
}

/// Nothing listening at all is a different answer from a server that refused,
/// and the probe must not present it as a status.
#[tokio::test]
async fn the_probe_reports_a_server_that_is_not_there() {
    // A port nothing is bound to: the fixture is started and immediately
    // dropped, which closes the listener.
    let base_url = {
        let server = Server::start(Vec::new()).await;
        server.base_url.clone()
    };

    let probe = openai::probe(
        openai::client().as_ref(),
        &probe_settings(&base_url),
        ApiKey::new("sk-test-key-abcd1234").as_ref(),
    )
    .await;

    assert!(!probe.ok);
    assert_eq!(
        probe.status, None,
        "nothing answered, so there is no status"
    );
    assert!(
        probe.message.contains("could not reach the server"),
        "{}",
        probe.message
    );
}
