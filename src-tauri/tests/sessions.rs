//! Sessions and turns, through the crate's public surface.
//!
//! Phase 5's walkthrough (PLAN 6): send, stream, survive a restart, cancel
//! mid-flight, using only `lib.rs` exports. Commands need a Tauri app;
//! everything below them runs here on temp files.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use tempfile::TempDir;

use aegis_lib::agent::event::EventSink;
use aegis_lib::agent::turn::{self, TurnPlan};
use aegis_lib::Standing;
use aegis_lib::{
    Agent, ApprovalRegistry, AuditLog, Event, FakeProvider, GrantStore, MemoryStore, Message, Role,
    SessionState, SessionStore, StopReason, Turn, TurnRegistry, DEFAULT_AGENT_ID,
};

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

    fn names(&self) -> Vec<&'static str> {
        self.events().iter().map(Event::name).collect()
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

/// A data directory, a workspace and the stores over them.
struct App {
    _dir: TempDir,
    data: PathBuf,
    /// An empty memory store: these files are about transcripts.
    memories: MemoryStore,
    workspace: PathBuf,
    sessions: SessionStore,
    turns: TurnRegistry,
    grants: GrantStore,
    approvals: ApprovalRegistry,
    audit: AuditLog,
}

impl App {
    fn new() -> Self {
        let dir = TempDir::new().expect("temp dir");
        let data = dir.path().join("data");
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&data).expect("data dir");
        std::fs::create_dir_all(&workspace).expect("workspace dir");

        Self {
            sessions: SessionStore::load(&data),
            memories: MemoryStore::load(&data),
            audit: AuditLog::new(&data),
            workspace: dunce::canonicalize(&workspace).expect("canonical workspace"),
            data,
            _dir: dir,
            turns: TurnRegistry::new(),
            grants: GrantStore::new(),
            approvals: ApprovalRegistry::new(),
        }
    }

    /// Reloads the session store, as restarting the process would.
    fn restart(&self) -> SessionStore {
        SessionStore::load(&self.data)
    }

    /// Registers a turn, runs it, and retires it the way the command does.
    async fn run(
        &self,
        session_id: &str,
        provider: &FakeProvider,
        sink: &Recorder,
        cancel_after: Option<Duration>,
    ) -> StopReason {
        let turn_id = format!("turn-{}", self.turns.active_turn(session_id).is_some());
        let cancel = self
            .turns
            .begin(session_id, &turn_id)
            .expect("the session is free");

        if let Some(delay) = cancel_after {
            let token = cancel.clone();
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                token.cancel();
            });
        }

        let plan = TurnPlan {
            session_id: session_id.to_owned(),
            turn_id: turn_id.clone(),
            workspace: Some(self.workspace.clone()),
            exec_host: None,
        };

        let reason = Turn {
            agent: &Agent::builtin(),
            sessions: &self.sessions,
            turns: &self.turns,
            grants: &self.grants,
            approvals: &self.approvals,
            audit: &self.audit,
            provider,
            sink,
            self_exe: None,
            captures: &self.data.join("captures"),
            skills: &self.data.join("skills"),
            memories: &self.memories,
            connectors: aegis_lib::Connectors::none(),
            standing: Standing::Own(None),
            unattended: None,
            parking: None,
            decision: None,
        }
        .run(&plan, &cancel)
        .await;

        self.turns
            .finish(session_id, &turn_id, turn::resting_state(reason));
        reason
    }
}

/// The Phase 5 walkthrough: a message goes in, a reply streams out, and the
/// whole exchange is still there after a restart.
#[tokio::test]
async fn a_conversation_streams_and_survives_a_restart() {
    let app = App::new();
    let session = app
        .sessions
        .create("project-1", None, DEFAULT_AGENT_ID)
        .expect("session created");

    assert_eq!(session.title, "New session");
    assert_eq!(session.state, SessionState::Idle);
    assert_eq!(session.message_count, 0);

    app.sessions
        .append(
            &session.id,
            Message::user("what workspace are you in?"),
            SessionState::Running,
        )
        .expect("the user's message is stored");

    let sink = Recorder::default();
    let reason = app
        .run(&session.id, &FakeProvider::instant(), &sink, None)
        .await;
    assert_eq!(reason, StopReason::Stop);

    // The lifecycle the UI depends on, in order.
    let names = sink.names();
    assert_eq!(names.first(), Some(&"turn:started"));
    assert_eq!(names.last(), Some(&"turn:finished"));
    assert!(names.contains(&"turn:message"));
    assert!(names.contains(&"session:updated"));

    // The session is idle again, and named after what was asked.
    let after = app
        .sessions
        .summary(&session.id, app.turns.state_of(&session.id))
        .expect("summary");
    assert_eq!(after.state, SessionState::Idle);
    assert_eq!(after.title, "what workspace are you in?");
    assert_eq!(after.message_count, 2);

    // Restart. The transcript is on disk; the running state was never there.
    let restarted = app.restart();
    let detail = restarted
        .open(&session.id, SessionState::Idle)
        .expect("the session reopens");

    assert_eq!(detail.session.state, SessionState::Idle);
    assert_eq!(detail.messages.len(), 2);
    assert_eq!(detail.messages[0].role, Role::User);
    assert_eq!(detail.messages[1].role, Role::Assistant);

    // What streamed is what was stored, and it reports what the request
    // actually carried — the workspace among it.
    assert_eq!(sink.streamed(), detail.messages[1].text);
    assert!(
        detail.messages[1]
            .text
            .contains(&app.workspace.display().to_string()),
        "the reply names the workspace it was given: {}",
        detail.messages[1].text
    );
}

/// Cancelling keeps what the user already saw, and leaves the session
/// immediately usable again.
#[tokio::test]
async fn cancelling_a_turn_leaves_the_session_ready_for_another() {
    let app = App::new();
    let session = app
        .sessions
        .create("project-1", None, DEFAULT_AGENT_ID)
        .expect("session");

    app.sessions
        .append(&session.id, Message::user("go"), SessionState::Running)
        .expect("append");

    // The pacing provider, so the cancel lands between two tokens.
    let sink = Recorder::default();
    let reason = app
        .run(
            &session.id,
            &FakeProvider::new(),
            &sink,
            Some(Duration::from_millis(60)),
        )
        .await;

    assert_eq!(reason, StopReason::Cancelled);
    assert_eq!(app.turns.state_of(&session.id), SessionState::Idle);
    assert_eq!(app.turns.active_turn(&session.id), None);

    let detail = app
        .sessions
        .open(&session.id, SessionState::Idle)
        .expect("open");
    let partial = detail.messages.last().expect("a partial reply");
    assert_eq!(partial.role, Role::Assistant);
    assert!(!partial.text.is_empty(), "what was on screen is kept");
    assert_eq!(sink.streamed(), partial.text);

    // And the session takes another turn straight away.
    let second = Recorder::default();
    app.sessions
        .append(&session.id, Message::user("again"), SessionState::Running)
        .expect("append");
    assert_eq!(
        app.run(&session.id, &FakeProvider::instant(), &second, None)
            .await,
        StopReason::Stop
    );
}

/// One session, one turn at a time — the invariant that keeps two replies from
/// interleaving in one transcript.
#[tokio::test]
async fn a_session_refuses_a_second_concurrent_turn() {
    let app = App::new();
    let session = app
        .sessions
        .create("project-1", None, DEFAULT_AGENT_ID)
        .expect("session");

    let cancel = app.turns.begin(&session.id, "t1").expect("the first turn");

    let err = app
        .turns
        .begin(&session.id, "t2")
        .expect_err("the second is refused");
    assert_eq!(err.code(), aegis_lib::ErrorCode::TurnBusy);

    // The refusal is per session, not global.
    let other = app
        .sessions
        .create("project-1", None, DEFAULT_AGENT_ID)
        .expect("session");
    assert!(app.turns.begin(&other.id, "t3").is_ok());

    cancel.cancel();
}

/// A tool call goes through policy, runs, and leaves exactly one audit line —
/// the same path Phase 6 will attach the approval dialog to.
#[tokio::test]
async fn a_tool_call_runs_under_policy_and_is_audited() {
    use aegis_lib::agent::wire::ModelEvent;

    let app = App::new();
    std::fs::write(app.workspace.join("notes.txt"), "two lines\nof text").expect("write");

    let session = app
        .sessions
        .create("project-1", None, DEFAULT_AGENT_ID)
        .expect("session");
    app.sessions
        .append(
            &session.id,
            Message::user("read notes.txt"),
            SessionState::Running,
        )
        .expect("append");

    let provider = FakeProvider::scripted(vec![vec![
        ModelEvent::ToolCallDelta {
            index: 0,
            id: Some("call_1".to_owned()),
            name: Some("fs_read".to_owned()),
            args_delta: r#"{"path":"notes.txt"}"#.to_owned(),
            thought_signature: None,
        },
        ModelEvent::Finish {
            reason: StopReason::ToolCalls,
            usage: None,
        },
    ]]);

    let sink = Recorder::default();
    assert_eq!(
        app.run(&session.id, &provider, &sink, None).await,
        StopReason::Stop
    );

    let names = sink.names();
    for expected in [
        "tool:requested",
        "tool:started",
        "tool:finished",
        "audit:appended",
    ] {
        assert!(names.contains(&expected), "missing {expected} in {names:?}");
    }

    // Exactly one line on disk, whatever the UI did with the event.
    let audit = app.audit.tail(10, None).expect("tail");
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].tool, "fs_read");
    assert_eq!(audit[0].session_id, session.id);
    assert_eq!(audit[0].outcome, aegis_lib::Outcome::Ok);

    // The transcript carries the answer the next request will need.
    let detail = app
        .sessions
        .open(&session.id, SessionState::Idle)
        .expect("open");
    let answered = detail
        .messages
        .iter()
        .any(|message| message.tool_call_id.as_deref() == Some("call_1"));
    assert!(answered, "every call is answered");
}

/// Deleting a project takes its transcripts with it; another project's are
/// untouched.
#[tokio::test]
async fn deleting_a_project_removes_only_its_sessions() {
    let app = App::new();
    app.sessions
        .create("gone", Some("one"), DEFAULT_AGENT_ID)
        .expect("session");
    app.sessions
        .create("gone", Some("two"), DEFAULT_AGENT_ID)
        .expect("session");
    let kept = app
        .sessions
        .create("stays", Some("three"), DEFAULT_AGENT_ID)
        .expect("session");

    assert_eq!(
        app.sessions.delete_for_project("gone").expect("delete"),
        2,
        "both sessions of the deleted project went"
    );

    let idle = |_: &str| SessionState::Idle;
    assert!(app.sessions.list("gone", &idle).is_empty());
    assert_eq!(app.sessions.list("stays", &idle).len(), 1);

    // And it is gone from disk, not only from memory.
    assert!(app.restart().open(&kept.id, SessionState::Idle).is_ok());
    assert!(app.restart().list("gone", &idle).is_empty());
}

/// A turn cancelled between "the model asked" and "the tool ran" must not
/// leave a call unanswered: the next request in that session would be
/// structurally invalid and every later turn would fail.
#[tokio::test]
async fn a_transcript_left_open_by_a_cancel_still_builds_a_valid_request() {
    use aegis_lib::agent::transcript;
    use aegis_lib::{ToolCallRecord, ToolCallStatus};

    let app = App::new();
    let session = app
        .sessions
        .create("project-1", None, DEFAULT_AGENT_ID)
        .expect("session");

    app.sessions
        .append(&session.id, Message::user("read it"), SessionState::Running)
        .expect("append");
    app.sessions
        .append(
            &session.id,
            Message::assistant(
                "",
                vec![ToolCallRecord {
                    call_id: "orphan".to_owned(),
                    tool: "fs_read".to_owned(),
                    args_json: r#"{"path":"a.txt"}"#.to_owned(),
                    status: ToolCallStatus::Cancelled,
                    summary: None,
                    image_path: None,
                    thought_signature: None,
                }],
            ),
            SessionState::Idle,
        )
        .expect("append");

    let history = app.sessions.messages(&session.id).expect("messages");
    let agent = Agent::builtin();
    let request = transcript::build(
        "m",
        &transcript::Context {
            agent: &agent,
            workspace: Some(&app.workspace),
            exec_host: None,
            memories: None,
            skills: None,
            world: None,
            shared: None,
            compacted: None,
            unattended: false,
        },
        &history,
        Vec::new(),
    );

    let answered = request.messages.iter().any(|message| {
        matches!(
            message,
            aegis_lib::agent::wire::WireMessage::Tool { tool_call_id, .. } if tool_call_id == "orphan"
        )
    });
    assert!(answered, "the orphaned call was answered on the way out");

    // And the session still takes a turn.
    let sink = Recorder::default();
    assert_eq!(
        app.run(&session.id, &FakeProvider::instant(), &sink, None)
            .await,
        StopReason::Stop
    );
}
