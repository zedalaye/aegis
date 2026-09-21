//! The approval gate, through the crate's public surface.
//!
//! Phase 6's exit conditions (PLAN 6) as the app assembles them: allow-once,
//! allow-session and deny; `E_DENIED` without ending the turn; grants listed and
//! revoked. Uses the improvising [`FakeProvider`] so its `/write` and `/run`
//! triggers stay tested. The `shell_exec` streaming half (Phase 7) is last.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use tempfile::TempDir;

use aegis_lib::agent::event::EventSink;
use aegis_lib::agent::provider::fake::{RUN_TRIGGER, WRITE_TARGET, WRITE_TRIGGER};
use aegis_lib::agent::turn::{self, TurnPlan};
use aegis_lib::Standing;
use aegis_lib::{
    Agent, ApprovalDecision, ApprovalRegistry, ApprovalRequest, AuditDecision, AuditLog, Event,
    FakeProvider, Grant, GrantStore, MemoryStore, Message, Outcome, ResolvedBy, SessionState,
    SessionStore, StopReason, ToolCallStatus, Turn, TurnRegistry, DEFAULT_AGENT_ID,
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

    fn count(&self, name: &str) -> usize {
        self.names().iter().filter(|found| **found == name).count()
    }

    /// Every approval this turn raised, in the order it raised them.
    fn asked(&self) -> Vec<ApprovalRequest> {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                Event::ToolApprovalRequired(request) => Some(*request),
                _ => None,
            })
            .collect()
    }

    /// Who closed each approval.
    fn resolutions(&self) -> Vec<(ApprovalDecision, ResolvedBy)> {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                Event::ToolApprovalResolved(resolved) => {
                    Some((resolved.decision, resolved.resolved_by))
                }
                _ => None,
            })
            .collect()
    }

    /// Everything a running tool printed, in the order the frames arrived.
    fn printed(&self) -> String {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                Event::ToolProgress(progress) => Some(progress.chunk),
                _ => None,
            })
            .collect()
    }

    /// The `seq` on each progress frame, in arrival order.
    fn progress_seqs(&self) -> Vec<u32> {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                Event::ToolProgress(progress) => Some(progress.seq),
                _ => None,
            })
            .collect()
    }

    /// The states the sidebar was told about, in order.
    fn states(&self) -> Vec<SessionState> {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                Event::SessionUpdated(summary) => Some(summary.state),
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

/// A data directory, a workspace, and every store the runtime holds.
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
    /// An empty skill library: these files are about other things.
    library: PathBuf,
    /// An empty memory store, for the same reason.
    memories: MemoryStore,
    /// The identity these turns run as: the built-in one, which holds every
    /// tool. What an allow-list does to a call is `tests/agents.rs`.
    agent: Agent,
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
            agent: Agent::builtin(),
        }
    }

    /// The runtime, assembled the way `session_send` assembles it.
    fn runtime<'a>(&'a self, provider: &'a FakeProvider, sink: &'a Recorder) -> Turn<'a> {
        Turn {
            agent: &self.agent,
            sessions: &self.sessions,
            turns: &self.turns,
            grants: &self.grants,
            approvals: &self.approvals,
            audit: &self.audit,
            provider,
            sink,
            self_exe: None,
            captures: &self.captures,
            skills: &self.library,
            memories: &self.memories,
            connectors: aegis_lib::Connectors::none(),
            standing: Standing::Own(None),
            unattended: None,
            parking: None,
            decision: None,
            meter: None,
        }
    }

    /// A plan for one turn of this session.
    fn plan(&self, turn_id: &str) -> TurnPlan {
        TurnPlan {
            session_id: self.session_id.clone(),
            turn_id: turn_id.to_owned(),
            workspace: Some(self.workspace.clone()),
            exec_host: None,
        }
    }

    /// The written file, if the write ever happened.
    fn written(&self) -> Option<String> {
        std::fs::read_to_string(self.workspace.join(WRITE_TARGET)).ok()
    }

    /// Runs one turn, answering each approval it raises with `answers` in
    /// order. An approval past the end of `answers` is left alone.
    ///
    /// Turn and answers run concurrently, as in the app.
    async fn turn(&self, sink: &Recorder, answers: &[ApprovalDecision]) -> StopReason {
        let turn_id = "turn-1";
        let cancel = self.turns.begin(&self.session_id, turn_id).expect("free");

        let plan = self.plan(turn_id);
        let provider = FakeProvider::instant();
        let turn = self.runtime(&provider, sink);
        let running = turn.run(&plan, &cancel);

        let answering = async {
            for decision in answers {
                let request = self.next_request().await;
                self.approvals
                    .resolve(&request.request_id, *decision, &self.grants)
                    .expect("the request is open");
            }
        };

        let (reason, ()) = tokio::join!(running, answering);
        self.turns
            .finish(&self.session_id, turn_id, turn::resting_state(reason));
        reason
    }

    /// Waits for the next approval the turn raises.
    async fn next_request(&self) -> ApprovalRequest {
        for _ in 0..500 {
            if let Some(request) = self
                .approvals
                .list(Some(&self.session_id))
                .into_iter()
                .next()
            {
                return request;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("no approval was raised");
    }

    /// Stores a message that makes the fake provider ask to run a command.
    fn ask_for_a_run(&self) {
        self.sessions
            .append(
                &self.session_id,
                Message::user(format!("{RUN_TRIGGER} something for me")),
                SessionState::Running,
            )
            .expect("the user's message is stored");
    }

    /// Stores a message that makes the fake provider ask to write a file.
    fn ask_for_a_write(&self) {
        self.sessions
            .append(
                &self.session_id,
                Message::user(format!("{WRITE_TRIGGER} a file for me")),
                SessionState::Running,
            )
            .expect("the user's message is stored");
    }
}

/// The Phase 6 walkthrough, allow-once branch: the model asks, the user
/// allows one call, the file appears, and nothing is left behind that would
/// skip the next prompt.
#[tokio::test]
async fn allowing_once_runs_the_call_and_leaves_no_grant() {
    let app = App::new();
    app.ask_for_a_write();

    let sink = Recorder::default();
    let reason = app.turn(&sink, &[ApprovalDecision::AllowOnce]).await;
    assert_eq!(reason, StopReason::Stop);

    // What the user was shown, before deciding.
    let asked = sink.asked();
    assert_eq!(asked.len(), 1);
    let request = &asked[0];
    assert_eq!(request.tool, "fs_write");
    assert_eq!(request.session_id, app.session_id);
    assert!(
        request.session_grant_allowed,
        "a contained write can be granted"
    );
    assert!(
        request.summary.contains(WRITE_TARGET),
        "the summary names the file: {}",
        request.summary
    );
    assert!(
        request.scope_label.contains("rest of this session"),
        "the scope is stated as a promise about the session: {}",
        request.scope_label
    );

    assert!(app.written().is_some(), "the approved write happened");
    assert_eq!(
        sink.resolutions(),
        vec![(ApprovalDecision::AllowOnce, ResolvedBy::User)]
    );

    assert!(
        app.grants.list(&app.session_id).is_empty(),
        "allow-once must not quietly become allow-session"
    );
    assert!(app.approvals.is_empty(), "nothing is still pending");

    let audit = app.audit.tail(10, None).expect("tail");
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].decision, AuditDecision::AllowOnce);
    assert_eq!(audit[0].outcome, Outcome::Ok);
    assert_eq!(audit[0].session_id, app.session_id);
}

/// The Phase 6 exit condition in full: a denial lands in the transcript as
/// `E_DENIED` without aborting the turn.
#[tokio::test]
async fn denying_refuses_the_call_and_the_turn_finishes_anyway() {
    let app = App::new();
    app.ask_for_a_write();

    let sink = Recorder::default();
    let reason = app.turn(&sink, &[ApprovalDecision::Deny]).await;

    assert_eq!(reason, StopReason::Stop, "a denial is not a failed turn");
    assert_eq!(app.written(), None, "nothing was written");
    assert!(
        !sink.names().contains(&"tool:started"),
        "a denied call never starts"
    );

    // The transcript: the call reads as denied, and the model was handed an
    // envelope it can act on rather than an exception.
    let detail = app
        .sessions
        .open(&app.session_id, SessionState::Idle)
        .expect("the session opens");
    let call = detail
        .messages
        .iter()
        .flat_map(|message| message.tool_calls.iter())
        .find(|call| call.tool == "fs_write")
        .expect("the call is in the transcript");
    assert_eq!(call.status, ToolCallStatus::Denied);

    let answer = detail
        .messages
        .iter()
        .find(|message| message.tool_call_id.as_deref() == Some(call.call_id.as_str()))
        .expect("the call was answered");
    let envelope: serde_json::Value = serde_json::from_str(&answer.text).expect("an envelope");
    assert_eq!(envelope["ok"], serde_json::json!(false));
    assert_eq!(envelope["error"]["code"], "E_DENIED");

    // And the turn kept going: the model got its round after the refusal.
    assert!(
        detail
            .messages
            .iter()
            .any(|message| message.created_at >= answer.created_at
                && message.tool_call_id.is_none()
                && !message.text.is_empty()),
        "the model answered after being refused"
    );

    let audit = app.audit.tail(10, None).expect("tail");
    assert_eq!(audit[0].decision, AuditDecision::Deny);
    assert_eq!(audit[0].outcome, Outcome::Denied);
    assert_eq!(audit[0].error_code.as_deref(), Some("E_DENIED"));
}

/// Allow-session: the grant is created, it is visible, and it can be taken
/// back — the second half of PLAN 3.1.
#[tokio::test]
async fn a_session_grant_is_visible_and_revocable() {
    let app = App::new();
    app.ask_for_a_write();

    let sink = Recorder::default();
    app.turn(&sink, &[ApprovalDecision::AllowSession]).await;

    // Visible, in the same words the dialog used.
    let granted = app.grants.list(&app.session_id);
    assert_eq!(granted, vec![Grant::FsWrite]);
    assert_eq!(granted[0].scope_label(), sink.asked()[0].scope_label);
    assert_eq!(granted[0].tool(), "fs_write");

    // A second turn is not asked about at all.
    app.ask_for_a_write();
    let second = Recorder::default();
    app.turn(&second, &[]).await;

    assert_eq!(second.count("tool:approval_required"), 0, "the grant held");
    let audit = app.audit.tail(10, None).expect("tail");
    assert_eq!(audit[0].decision, AuditDecision::Auto);

    // Revocable, and idempotently so.
    assert!(app.grants.revoke(&app.session_id, &Grant::FsWrite));
    assert!(!app.grants.revoke(&app.session_id, &Grant::FsWrite));
    assert!(app.grants.list(&app.session_id).is_empty());

    // And the ask comes back.
    app.ask_for_a_write();
    let third = Recorder::default();
    app.turn(&third, &[ApprovalDecision::Deny]).await;
    assert_eq!(
        third.count("tool:approval_required"),
        1,
        "revoking a grant restores the prompt"
    );
}

/// A grant belongs to the session that made it, and to no other. Two sessions
/// on one workspace are two conversations, and approving something in one is
/// not approving it in the other.
#[tokio::test]
async fn a_grant_does_not_leak_into_another_session() {
    let app = App::new();
    app.ask_for_a_write();
    app.turn(&Recorder::default(), &[ApprovalDecision::AllowSession])
        .await;

    let other = app
        .sessions
        .create("project-1", Some("another conversation"), DEFAULT_AGENT_ID)
        .expect("a second session");

    assert!(app.grants.holds(&app.session_id, &Grant::FsWrite));
    assert!(
        !app.grants.holds(&other.id, &Grant::FsWrite),
        "the second session was never asked, so it granted nothing"
    );
    assert!(app.grants.list(&other.id).is_empty());
}

/// A window that was closed while a dialog was open has to be able to find it
/// again: the pending request is on the session detail, not only on the event
/// that raised it.
#[tokio::test]
async fn a_reopened_session_still_shows_what_it_is_blocked_on() {
    let app = App::new();
    app.ask_for_a_write();

    let turn_id = "turn-1";
    let cancel = app.turns.begin(&app.session_id, turn_id).expect("free");
    let plan = app.plan(turn_id);
    let provider = FakeProvider::instant();
    let sink = Recorder::default();
    let turn = app.runtime(&provider, &sink);
    let running = turn.run(&plan, &cancel);

    let inspecting = async {
        let request = app.next_request().await;

        // What `session_open` composes while the turn is parked.
        let state = app.turns.state_of(&app.session_id);
        let pending = app.approvals.list(Some(&app.session_id));

        app.approvals
            .resolve(&request.request_id, ApprovalDecision::Deny, &app.grants)
            .expect("the request is open");
        (state, pending)
    };

    let (reason, (state, pending)) = tokio::join!(running, inspecting);
    app.turns
        .finish(&app.session_id, turn_id, turn::resting_state(reason));

    assert_eq!(
        state,
        SessionState::AwaitingApproval,
        "a session waiting for a person does not read as working"
    );
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].tool, "fs_write");
    assert_eq!(pending[0].call_id, sink.asked()[0].call_id);

    // And the badge went back once it was answered.
    assert!(sink.states().contains(&SessionState::AwaitingApproval));
    assert_eq!(app.turns.state_of(&app.session_id), SessionState::Idle);
    assert!(app.approvals.is_empty());
}

/// Answering a request twice is not a way to run a call twice. The second
/// answer finds nothing waiting on it.
#[tokio::test]
async fn an_approval_cannot_be_answered_twice() {
    let app = App::new();
    app.ask_for_a_write();

    let turn_id = "turn-1";
    let cancel = app.turns.begin(&app.session_id, turn_id).expect("free");
    let plan = app.plan(turn_id);
    let provider = FakeProvider::instant();
    let sink = Recorder::default();
    let turn = app.runtime(&provider, &sink);
    let running = turn.run(&plan, &cancel);

    let answering = async {
        let request = app.next_request().await;
        app.approvals
            .resolve(
                &request.request_id,
                ApprovalDecision::AllowOnce,
                &app.grants,
            )
            .expect("the first answer wins");
        app.approvals
            .resolve(
                &request.request_id,
                ApprovalDecision::AllowOnce,
                &app.grants,
            )
            .expect_err("the second finds nothing to answer")
    };

    let (reason, err) = tokio::join!(running, answering);
    app.turns
        .finish(&app.session_id, turn_id, turn::resting_state(reason));

    assert_eq!(err.code(), aegis_lib::ErrorCode::ApprovalStale);
    let audit = app.audit.tail(10, None).expect("tail");
    assert_eq!(audit.len(), 1, "the call ran once");
}

/// Deleting a session takes its approvals and its grants with it. A dialog
/// left answerable for a session that no longer exists would be a button that
/// approves a call nothing can run.
#[tokio::test]
async fn deleting_a_session_drops_what_it_was_waiting_on() {
    let app = App::new();
    app.ask_for_a_write();
    app.turn(&Recorder::default(), &[ApprovalDecision::AllowSession])
        .await;
    assert!(!app.grants.list(&app.session_id).is_empty());

    app.turns.forget(&app.session_id);
    app.approvals.withdraw_session(&app.session_id);
    app.grants.clear(&app.session_id);
    app.sessions.delete(&app.session_id).expect("deleted");

    assert!(app.grants.list(&app.session_id).is_empty());
    assert!(app.approvals.list(Some(&app.session_id)).is_empty());
}

// ---------------------------------------------------------------------------
// Phase 7 — the shell tool through the same gate
// ---------------------------------------------------------------------------

/// The Phase 7 walkthrough (PLAN § 6): the model asks to run a command, the
/// user reads the exact program, arguments and working directory, allows it
/// once, and the output arrives as `tool:progress` while the command is still
/// running rather than in one lump at the end.
#[tokio::test]
async fn a_command_runs_under_approval_and_streams_its_output() {
    let app = App::new();
    app.ask_for_a_run();

    let sink = Recorder::default();
    let reason = app.turn(&sink, &[ApprovalDecision::AllowOnce]).await;
    assert_eq!(reason, StopReason::Stop);

    // What the user was shown before deciding. The dialog carries the command
    // in pieces, not as a string that something would have to re-split.
    let asked = sink.asked();
    assert_eq!(asked.len(), 1);
    let request = &asked[0];
    assert_eq!(request.tool, "shell_exec");
    assert_eq!(request.title, "Run shell command");
    assert!(
        request.session_grant_allowed,
        "a command in the workspace can be granted for the session"
    );
    assert!(
        request.scope_label.contains("rest of this session"),
        "{}",
        request.scope_label
    );
    match &request.detail {
        aegis_lib::policy::ApprovalDetail::Shell {
            program,
            cwd,
            shell_line,
            ..
        } => {
            assert!(!program.is_empty());
            assert_eq!(cwd, &app.workspace.display().to_string());
            assert!(shell_line.starts_with(program), "{shell_line}");
        }
        other => panic!("a shell call must render as a shell detail, got {other:?}"),
    }

    // It ran, and the pane saw it run. The command lists the workspace, so its
    // output names the file the earlier phases' demo writes into it — proof
    // the frames are the command's own output and not a placeholder.
    assert_eq!(sink.count("tool:started"), 1);
    assert!(sink.count("tool:progress") > 0, "the output was streamed");
    assert!(
        !sink.printed().is_empty(),
        "the frames carried the command's own output"
    );

    // `seq` is per-turn and increasing, which is what lets the UI drop a
    // duplicated or reordered frame.
    let seqs = sink.progress_seqs();
    assert!(
        seqs.windows(2).all(|pair| pair[0] < pair[1]),
        "progress frames are numbered in order: {seqs:?}"
    );

    let detail = app
        .sessions
        .open(&app.session_id, SessionState::Idle)
        .expect("the session opens");
    let call = detail
        .messages
        .iter()
        .flat_map(|message| message.tool_calls.iter())
        .find(|call| call.tool == "shell_exec")
        .expect("the call is in the transcript");
    assert_eq!(call.status, ToolCallStatus::Ok);
    assert!(
        call.summary
            .as_deref()
            .is_some_and(|line| line.contains("ms")),
        "the transcript keeps one line, not the output: {:?}",
        call.summary
    );

    // The model saw an envelope with an exit code in it, not a bare string.
    let answer = detail
        .messages
        .iter()
        .find(|message| message.tool_call_id.as_deref() == Some(call.call_id.as_str()))
        .expect("the call was answered");
    let envelope: serde_json::Value = serde_json::from_str(&answer.text).expect("an envelope");
    assert_eq!(envelope["ok"], serde_json::json!(true), "{envelope}");
    assert_eq!(envelope["meta"]["exit_code"], serde_json::json!(0));

    let audit = app.audit.tail(10, None).expect("tail");
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].tool, "shell_exec");
    assert_eq!(audit[0].decision, AuditDecision::AllowOnce);
    assert_eq!(audit[0].outcome, Outcome::Ok);
}

/// A refused command never starts, so there is nothing to stream. The turn
/// carries on, exactly as it does for a refused write.
#[tokio::test]
async fn a_refused_command_never_runs_and_prints_nothing() {
    let app = App::new();
    app.ask_for_a_run();

    let sink = Recorder::default();
    let reason = app.turn(&sink, &[ApprovalDecision::Deny]).await;

    assert_eq!(reason, StopReason::Stop, "a denial is not a failed turn");
    assert_eq!(sink.count("tool:started"), 0);
    assert_eq!(
        sink.count("tool:progress"),
        0,
        "nothing ran, so nothing printed"
    );

    let audit = app.audit.tail(10, None).expect("tail");
    assert_eq!(audit[0].tool, "shell_exec");
    assert_eq!(audit[0].outcome, Outcome::Denied);
    assert_eq!(audit[0].error_code.as_deref(), Some("E_DENIED"));
}

/// PLAN 3.1: the shell grant is keyed on the program, and it says so in the
/// words the dialog used.
#[tokio::test]
async fn a_command_granted_for_the_session_names_the_program_it_covers() {
    let app = App::new();
    app.ask_for_a_run();

    let sink = Recorder::default();
    app.turn(&sink, &[ApprovalDecision::AllowSession]).await;

    let granted = app.grants.list(&app.session_id);
    assert_eq!(granted.len(), 1);
    assert_eq!(granted[0].tool(), "shell_exec");
    assert_eq!(granted[0].scope_label(), sink.asked()[0].scope_label);
    assert!(
        matches!(&granted[0], Grant::Shell { program } if !program.is_empty()),
        "the grant is one program, not the tool: {:?}",
        granted[0]
    );

    // A second turn asking for the same command is not put to the user again.
    app.ask_for_a_run();
    let second = Recorder::default();
    app.turn(&second, &[]).await;

    assert_eq!(second.count("tool:approval_required"), 0, "the grant held");
    assert!(second.count("tool:progress") > 0, "and it still ran");
    let audit = app.audit.tail(10, None).expect("tail");
    assert_eq!(audit[0].decision, AuditDecision::Auto);
}
