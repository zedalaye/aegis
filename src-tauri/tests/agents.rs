//! The agent registry, through the crate's public surface.
//!
//! Phase 12's exit condition (PLAN 7.3): a "reviewer" identity cannot see or
//! use tools it was not granted.
//!
//! 1. Ungranted schemas are not sent.
//! 2. Ungranted calls are refused by policy, with no dialog.
//! 3. The audit line names the identity.
//!
//! And a session naming no identity behaves as in Phase 11. Commands need a
//! Tauri app; everything below them runs here on temp files.

use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::json;
use tempfile::TempDir;

use aegis_lib::agent::event::EventSink;
use aegis_lib::agent::transcript;
use aegis_lib::agent::turn::TurnPlan;
use aegis_lib::agent::wire::{ModelEvent, StopReason, WireMessage};
use aegis_lib::policy::tool;
use aegis_lib::Standing;
use aegis_lib::{
    Agent, AgentDraft, AgentStore, ApprovalRegistry, AuditLog, Event, FakeProvider, GrantStore,
    MemoryStore, Message, SessionState, SessionStore, ToolCallStatus, Turn, TurnRegistry,
    DEFAULT_AGENT_ID, DEFAULT_PROVIDER_ID,
};

/// Collects every event a turn emits.
#[derive(Debug, Default)]
struct Recorder {
    events: Mutex<Vec<Event>>,
}

impl Recorder {
    fn names(&self) -> Vec<&'static str> {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .map(Event::name)
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

/// A data directory, a workspace, and every store the runtime holds.
struct App {
    _dir: TempDir,
    workspace: PathBuf,
    agents: AgentStore,
    sessions: SessionStore,
    turns: TurnRegistry,
    grants: GrantStore,
    approvals: ApprovalRegistry,
    audit: AuditLog,
    captures: PathBuf,
    /// An empty skill library: these files are about other things.
    library: PathBuf,
    /// An empty memory store, for the same reason.
    memories: MemoryStore,
}

impl App {
    fn new() -> Self {
        let dir = TempDir::new().expect("temp dir");
        let data = dir.path().join("data");
        let workspace = dir.path().join("work");
        std::fs::create_dir_all(&data).expect("data dir");
        std::fs::create_dir_all(&workspace).expect("workspace dir");
        std::fs::write(workspace.join("notes.md"), "already here").expect("a file to read");

        Self {
            workspace: dunce::canonicalize(&workspace).expect("canonical workspace"),
            _dir: dir,
            agents: AgentStore::load(&data),
            sessions: SessionStore::load(&data),
            turns: TurnRegistry::new(),
            grants: GrantStore::new(),
            approvals: ApprovalRegistry::new(),
            audit: AuditLog::new(&data),
            captures: data.join("captures"),
            library: data.join("skills"),
            memories: MemoryStore::load(&data),
        }
    }

    /// An identity that may look and read, and may not change anything.
    fn reviewer(&self) -> Agent {
        self.agents
            .create(&AgentDraft {
                name: "Reviewer".to_owned(),
                role: "reads the workspace and reports what is risky".to_owned(),
                instructions: "Quote the line you are worried about.".to_owned(),
                provider_id: DEFAULT_PROVIDER_ID.to_owned(),
                tools: vec![tool::FS_LIST.to_owned(), tool::FS_READ.to_owned()],
                skills: Vec::new(),
                runs_per_day: 24,
            })
            .expect("the identity is accepted")
    }

    /// A session opened as `agent`, the way `session_create` opens one.
    fn session_as(&self, agent: &Agent) -> String {
        self.sessions
            .create("project-1", None, &agent.id)
            .expect("session")
            .id
    }

    /// The request the next turn of `session_id` would send, built as the turn
    /// loop builds it.
    fn next_request(&self, session_id: &str, agent: &Agent) -> (String, Vec<String>) {
        let history = self.sessions.messages(session_id).expect("messages");
        let request = transcript::build(
            "m",
            &transcript::Context {
                agent,
                workspace: Some(&self.workspace),
                exec_host: None,
                memories: None,
                skills: None,
                world: None,
                shared: None,
                compacted: None,
                unattended: false,
            },
            &history,
            aegis_lib::tools::schemas_for(&agent.tools, &aegis_lib::ConnectorCatalog::empty()),
        );

        let system = match request.messages.first() {
            Some(WireMessage::System { content }) => content.clone(),
            other => panic!("the first message is not a system message: {other:?}"),
        };
        let offered = request
            .tools
            .iter()
            .map(|spec| spec["function"]["name"].as_str().unwrap_or("?").to_owned())
            .collect();

        (system, offered)
    }

    /// Runs one turn in which the model asks for exactly one tool call.
    ///
    /// Nothing answers an approval here, deliberately: every call this file
    /// makes is either auto-allowed or refused outright, and a turn that parked
    /// on a dialog would be the failure, not the setup.
    async fn call_turn(
        &self,
        session_id: &str,
        agent: &Agent,
        sink: &Recorder,
        name: &str,
        args: serde_json::Value,
    ) -> StopReason {
        let turn_id = "turn-1";
        let cancel = self.turns.begin(session_id, turn_id).expect("free");

        let provider = FakeProvider::scripted(vec![vec![
            ModelEvent::ToolCallDelta {
                index: 0,
                id: Some("call_1".to_owned()),
                name: Some(name.to_owned()),
                args_delta: args.to_string(),
                thought_signature: None,
            },
            ModelEvent::Finish {
                reason: StopReason::ToolCalls,
                usage: None,
            },
        ]]);

        let plan = TurnPlan {
            session_id: session_id.to_owned(),
            turn_id: turn_id.to_owned(),
            workspace: Some(self.workspace.clone()),
            exec_host: None,
        };
        let reason = Turn {
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
            connectors: aegis_lib::Connectors::none(),
            standing: Standing::Own(None),
            unattended: None,
        }
        .run(&plan, &cancel)
        .await;

        self.turns.finish(session_id, turn_id, SessionState::Idle);
        reason
    }

    fn say(&self, session_id: &str, text: &str) {
        self.sessions
            .append(session_id, Message::user(text), SessionState::Running)
            .expect("the user's message is stored");
    }

    /// The status of the one tool call in the transcript.
    fn call_status(&self, session_id: &str) -> ToolCallStatus {
        self.sessions
            .messages(session_id)
            .expect("messages")
            .iter()
            .flat_map(|message| message.tool_calls.clone())
            .next()
            .expect("the call is in the transcript")
            .status
    }

    /// The envelope the model was handed for that call.
    fn envelope(&self, session_id: &str) -> serde_json::Value {
        let messages = self.sessions.messages(session_id).expect("messages");
        let tool = messages
            .iter()
            .find(|message| message.tool_call_id.is_some())
            .expect("the call was answered");

        serde_json::from_str(&tool.text).expect("an envelope the model can parse")
    }
}

/// Claim 1: the reviewer is not shown the tools it does not hold, and *is*
/// shown the ones it does.
#[test]
fn a_session_opened_as_an_identity_is_offered_only_its_own_tools() {
    let app = App::new();
    let reviewer = app.reviewer();
    let session_id = app.session_as(&reviewer);
    app.say(&session_id, "what is in here");

    let (system, offered) = app.next_request(&session_id, &reviewer);

    assert_eq!(
        offered,
        vec![tool::FS_LIST, tool::FS_READ],
        "the tools array carries the allow-list, in registry order"
    );
    assert!(
        !offered.iter().any(|name| name == tool::FS_WRITE),
        "a tool it was not granted is not a tool it is told about"
    );

    // And the identity is who the model is told it is.
    assert!(system.contains("Reviewer"), "{system}");
    assert!(system.contains("reads the workspace"), "{system}");
    assert!(system.contains("Quote the line"), "{system}");
}

/// Claim 2: asking anyway is refused before anything touches the machine, and
/// no approval is offered — an identity is not something a user can be prompted
/// past.
#[tokio::test]
async fn a_tool_it_was_not_granted_is_refused_without_a_prompt() {
    let app = App::new();
    let reviewer = app.reviewer();
    let session_id = app.session_as(&reviewer);
    app.say(&session_id, "fix it for me");

    let sink = Recorder::default();
    let reason = app
        .call_turn(
            &session_id,
            &reviewer,
            &sink,
            tool::FS_WRITE,
            json!({ "path": "notes.md", "content": "rewritten" }),
        )
        .await;

    assert_eq!(reason, StopReason::Stop, "the turn finishes cleanly");
    assert!(
        !sink.names().contains(&"tool:approval_required"),
        "a dialog asking to exceed an allow-list is a dialog that should not exist: {:?}",
        sink.names()
    );
    assert!(
        !sink.names().contains(&"tool:started"),
        "and nothing ran: {:?}",
        sink.names()
    );

    // A denial is a result, not an exception (PLAN 4.3): the model gets an
    // ordinary envelope naming the identity, and can say what it was trying to
    // do instead of stalling.
    let envelope = app.envelope(&session_id);
    assert_eq!(envelope["ok"], json!(false));
    assert_eq!(envelope["error"]["code"], "E_DENIED");
    let message = envelope["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("Reviewer"), "{message}");
    assert!(message.contains(tool::FS_WRITE), "{message}");

    assert_eq!(app.call_status(&session_id), ToolCallStatus::Denied);
    // The file the model wanted to rewrite is untouched.
    assert_eq!(
        std::fs::read_to_string(app.workspace.join("notes.md")).expect("read"),
        "already here"
    );
}

/// Claim 3: the record says which identity was refused. Without it, an audit
/// line cannot be read back against the grant that was supposed to allow it.
#[tokio::test]
async fn the_audit_line_names_the_identity_that_made_the_call() {
    let app = App::new();
    let reviewer = app.reviewer();
    let session_id = app.session_as(&reviewer);
    app.say(&session_id, "have a look, then fix it");

    let sink = Recorder::default();
    app.call_turn(
        &session_id,
        &reviewer,
        &sink,
        tool::FS_READ,
        json!({ "path": "notes.md" }),
    )
    .await;

    let allowed = app.audit.tail(10, None).expect("tail");
    let read = allowed.first().expect("the read was audited");
    assert_eq!(read.tool, tool::FS_READ);
    assert_eq!(read.agent_id, reviewer.id);
    assert_eq!(read.outcome, aegis_lib::Outcome::Ok);

    let refused_session = app.session_as(&reviewer);
    app.say(&refused_session, "fix it");
    app.call_turn(
        &refused_session,
        &reviewer,
        &Recorder::default(),
        tool::SHELL_EXEC,
        json!({ "program": "git", "args": ["status"] }),
    )
    .await;

    let lines = app.audit.tail(10, None).expect("tail");
    let refusal = lines
        .iter()
        .find(|entry| entry.tool == tool::SHELL_EXEC)
        .expect("the refusal was audited too");
    assert_eq!(refusal.agent_id, reviewer.id);
    assert_eq!(refusal.outcome, aegis_lib::Outcome::Denied);
    assert!(
        refusal.policy_reason.contains("Reviewer"),
        "{}",
        refusal.policy_reason
    );
}

/// The tools an identity *does* hold are judged by the ordinary matrix, not by
/// a second one. A read inside the workspace is auto-allowed for a reviewer
/// exactly as it is for the default identity.
#[tokio::test]
async fn a_granted_tool_is_gated_by_the_ordinary_matrix() {
    let app = App::new();
    let reviewer = app.reviewer();
    let session_id = app.session_as(&reviewer);
    app.say(&session_id, "read the notes");

    let sink = Recorder::default();
    app.call_turn(
        &session_id,
        &reviewer,
        &sink,
        tool::FS_READ,
        json!({ "path": "notes.md" }),
    )
    .await;

    assert!(
        !sink.names().contains(&"tool:approval_required"),
        "a contained read is auto-allowed, allow-list or not: {:?}",
        sink.names()
    );
    assert_eq!(app.call_status(&session_id), ToolCallStatus::Ok);
    assert_eq!(app.envelope(&session_id)["ok"], json!(true));
}

/// The other direction, and the reason this phase is safe to land: a session
/// written before identities existed opens, resolves, and behaves exactly as it
/// did in Phase 11.
///
/// The document is hand-written: the store can no longer produce one.
#[test]
fn a_session_written_before_identities_is_the_assistant_it_always_was() {
    let dir = TempDir::new().expect("temp dir");
    let data = dir.path().join("data");
    let workspace = dir.path().join("work");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&workspace).expect("workspace dir");

    std::fs::write(
        data.join("sessions.json"),
        json!({
            "version": 1,
            "sessions": [{
                "id": "session-from-phase-11",
                "project_id": "project-1",
                "title": "Before identities",
                "created_at": "2026-08-28T09:41:07.412Z",
                "updated_at": "2026-08-28T09:41:07.412Z",
                "messages": [],
            }],
        })
        .to_string(),
    )
    .expect("a document with no agent_id, as an earlier build wrote it");

    let sessions = SessionStore::load(&data);
    let agents = AgentStore::load(&data);

    let summary = sessions
        .summary("session-from-phase-11", SessionState::Idle)
        .expect("it still opens");
    assert_eq!(
        summary.agent_id, DEFAULT_AGENT_ID,
        "the payload always names an identity, even when the document did not"
    );
    assert_eq!(
        sessions.agent_of("session-from-phase-11").expect("stored"),
        None,
        "and the document is not rewritten to say otherwise"
    );

    let resolved = agents.resolve(None);
    assert_eq!(resolved, Agent::builtin());

    sessions
        .append(
            "session-from-phase-11",
            Message::user("hello"),
            SessionState::Running,
        )
        .expect("stored");
    let history = sessions
        .messages("session-from-phase-11")
        .expect("messages");
    let request = transcript::build(
        "m",
        &transcript::Context {
            agent: &resolved,
            workspace: Some(&workspace),
            exec_host: None,
            memories: None,
            skills: None,
            world: None,
            shared: None,
            compacted: None,
            unattended: false,
        },
        &history,
        aegis_lib::tools::schemas_for(&resolved.tools, &aegis_lib::ConnectorCatalog::empty()),
    );

    let offered: Vec<&str> = request
        .tools
        .iter()
        .map(|spec| spec["function"]["name"].as_str().unwrap_or("?"))
        .collect();
    assert_eq!(offered, aegis_lib::tools::names(), "every tool, as before");

    match request.messages.first() {
        Some(WireMessage::System { content }) => {
            assert!(!content.contains("You are working as"), "{content}");
            assert!(!content.contains("holds no tools"), "{content}");
        }
        other => panic!("the first message is not a system message: {other:?}"),
    }
}

/// Deleting an identity out from under its sessions would rewrite what they
/// were, so it is refused and the count is named.
#[test]
fn an_identity_cannot_be_deleted_while_a_session_still_runs_as_it() {
    let dir = TempDir::new().expect("temp dir");
    let state = aegis_lib::AppState::new(dir.path());

    let reviewer = state
        .agents()
        .create(&AgentDraft {
            name: "Reviewer".to_owned(),
            role: "reads and reports".to_owned(),
            instructions: String::new(),
            provider_id: DEFAULT_PROVIDER_ID.to_owned(),
            tools: vec![tool::FS_READ.to_owned()],
            skills: Vec::new(),
            runs_per_day: 24,
        })
        .expect("created");
    let session = state
        .create_session("project-1", None, Some(&reviewer.id))
        .expect("session");

    // Through `AppState`, not the store: the store cannot see the session
    // document, so this refusal only exists at the level that can, and the
    // panel's Remove button is the only thing in front of it.
    let err = state.delete_agent(&reviewer.id).expect_err("refused");
    assert!(
        err.to_string().contains('1'),
        "the count is named, so the user knows what to delete first: {err}"
    );
    assert!(
        state.agents().get(&reviewer.id).is_ok(),
        "and the identity is kept, not half-removed"
    );

    // Refused, never cascaded: the session is still bound to it, and still its
    // own record of what that identity did.
    assert_eq!(
        state.agent_of(&session.id),
        reviewer,
        "the refusal left the binding alone"
    );

    state.sessions().delete(&session.id).expect("deleted");
    state
        .delete_agent(&reviewer.id)
        .expect("nothing runs as it any more");
    assert!(state.agents().get(&reviewer.id).is_err());
}

/// The built-in identity is refused before the session count is even looked
/// at: it is not a record, so there is nothing a user could delete first that
/// would make it go.
#[test]
fn the_builtin_identity_is_refused_even_with_nothing_bound_to_it() {
    let dir = TempDir::new().expect("temp dir");
    let state = aegis_lib::AppState::new(dir.path());

    let err = state.delete_agent(DEFAULT_AGENT_ID).expect_err("refused");
    assert!(err.to_string().contains("built-in"), "{err}");
    assert_eq!(state.agent_list().len(), 1);
}

/// An identity, and a session's binding to it, survive a restart of the whole
/// runtime — not just of the store it lives in.
///
/// At the `AppState` level, above the store's own round-trip test.
#[test]
fn an_identity_and_its_sessions_survive_a_restart_of_the_runtime() {
    let dir = TempDir::new().expect("temp dir");

    let first = aegis_lib::AppState::new(dir.path());
    let reviewer = first
        .agents()
        .create(&AgentDraft {
            name: "Reviewer".to_owned(),
            role: "reads and reports".to_owned(),
            instructions: String::new(),
            provider_id: DEFAULT_PROVIDER_ID.to_owned(),
            tools: vec![tool::FS_READ.to_owned()],
            skills: Vec::new(),
            runs_per_day: 24,
        })
        .expect("created");
    let session = first
        .create_session("project-1", None, Some(&reviewer.id))
        .expect("session");
    drop(first);

    let second = aegis_lib::AppState::new(dir.path());

    assert_eq!(
        second.agent_list().len(),
        2,
        "the built-in identity and the one that was created"
    );
    let reloaded = second.agents().get(&reviewer.id).expect("still on file");
    assert_eq!(reloaded, reviewer);
    assert_eq!(second.agent_of(&session.id), reviewer);

    // And a third construction, because the failure being chased was a file
    // that had been emptied between two runs rather than by either of them.
    let third = aegis_lib::AppState::new(dir.path());
    assert_eq!(third.agent_list().len(), 2);
    assert!(
        std::fs::read_to_string(dir.path().join("agents.json"))
            .expect("read")
            .contains("Reviewer"),
        "opening the store must never rewrite it"
    );
}
