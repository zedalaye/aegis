//! The shared-workspace convention, through the crate's public surface.
//!
//! Phase 11's exit condition (PLAN 7.3): decisions and status are filed with
//! existing tools, no new agent type.
//!
//! 1. Scaffolding is opt-in and never overwrites.
//! 2. The files reach the next request's system message.
//! 3. A gated `fs_write` of `DECISIONS.md` is audited, durable and visible next
//!    request, and nothing is committed (PLAN 7.11).
//!
//! Commands need a Tauri app; everything below them runs here on temp files.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::json;
use tempfile::TempDir;

use aegis_lib::agent::event::EventSink;
use aegis_lib::agent::transcript;
use aegis_lib::agent::turn::{self, TurnPlan};
use aegis_lib::agent::wire::{ModelEvent, StopReason, WireMessage};
use aegis_lib::workspace::{self, DECISIONS_FILE, STATUS_FILE};
use aegis_lib::Standing;
use aegis_lib::{
    Agent, ApprovalDecision, ApprovalRegistry, ApprovalRequest, AuditLog, Event, FakeProvider,
    GrantStore, MemoryStore, Message, SessionState, SessionStore, Turn, TurnRegistry,
    DEFAULT_AGENT_ID,
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

    /// The system message the *next* request would carry.
    ///
    /// Assembled the way the turn loop assembles it — read the files, build the
    /// request — because the claim under test is that the loop does this, not
    /// that a helper exists.
    fn next_system_message(&self) -> String {
        let history = self.sessions.messages(&self.session_id).expect("messages");
        let shared = workspace::digest(&self.workspace);
        let agent = Agent::builtin();
        let request = transcript::build(
            "m",
            &transcript::Context {
                agent: &agent,
                workspace: Some(&self.workspace),
                exec_host: None,
                memories: None,
                skills: None,
                world: None,
                shared: shared.as_deref(),
                compacted: None,
                unattended: false,
            },
            &history,
            Vec::new(),
        );

        match request.messages.first() {
            Some(WireMessage::System { content }) => content.clone(),
            other => panic!("the first message is not a system message: {other:?}"),
        }
    }

    /// Runs one scripted turn, answering the approval it raises.
    async fn write_turn(&self, sink: &Recorder, path: &str, content: &str) -> StopReason {
        let turn_id = "turn-1";
        let cancel = self.turns.begin(&self.session_id, turn_id).expect("free");

        let arguments = json!({ "path": path, "content": content }).to_string();
        let provider = FakeProvider::scripted(vec![vec![
            ModelEvent::ToolCallDelta {
                index: 0,
                id: Some("call_1".to_owned()),
                name: Some("fs_write".to_owned()),
                args_delta: arguments,
                thought_signature: None,
            },
            ModelEvent::Finish {
                reason: StopReason::ToolCalls,
                usage: None,
            },
        ]]);

        let plan = TurnPlan {
            session_id: self.session_id.clone(),
            turn_id: turn_id.to_owned(),
            workspace: Some(self.workspace.clone()),
            exec_host: None,
        };
        let turn = Turn {
            agent: &Agent::builtin(),
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
            parking: None,
            decision: None,
        };

        let running = turn.run(&plan, &cancel);
        let answering = async {
            let request = self.next_request().await;
            self.approvals
                .resolve(
                    &request.request_id,
                    ApprovalDecision::AllowOnce,
                    &self.grants,
                )
                .expect("the request is open");
        };

        let (reason, ()) = tokio::join!(running, answering);
        self.turns
            .finish(&self.session_id, turn_id, turn::resting_state(reason));
        reason
    }

    /// Waits for the approval the turn raises.
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

    fn say(&self, text: &str) {
        self.sessions
            .append(&self.session_id, Message::user(text), SessionState::Running)
            .expect("the user's message is stored");
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.workspace.join(rel)).expect("the file is there")
    }
}

// ---------------------------------------------------------------------------
// 1. Opt-in, and never destructive
// ---------------------------------------------------------------------------

/// A workspace nobody opted in behaves exactly as it did before Phase 11 —
/// including in the prompt, which is where an unwanted convention would be
/// most expensive and least visible.
#[test]
fn a_workspace_is_left_alone_until_someone_asks() {
    let app = App::new();

    assert!(!workspace::layout(&app.workspace).complete);
    assert_eq!(workspace::digest(&app.workspace), None);
    assert!(!app.next_system_message().contains("DECISIONS.md"));

    let entries: Vec<_> = std::fs::read_dir(&app.workspace)
        .expect("read the workspace")
        .collect();
    assert!(entries.is_empty(), "nothing was written into the folder");
}

/// Scaffolding is idempotent, and a file the user already keeps is theirs.
#[test]
fn scaffolding_lays_down_what_is_missing_and_keeps_what_is_not() {
    let app = App::new();
    std::fs::create_dir_all(app.workspace.join(".aegis/decisions")).expect("decisions dir");
    std::fs::write(app.workspace.join(DECISIONS_FILE), "# Decisions\n\nours\n").expect("write");

    let report = workspace::scaffold(&app.workspace).expect("scaffolded");

    assert_eq!(report.kept, vec![DECISIONS_FILE]);
    assert!(report.created.contains(&STATUS_FILE.to_owned()));
    assert_eq!(app.read(DECISIONS_FILE), "# Decisions\n\nours\n");
    assert!(workspace::layout(&app.workspace).complete);

    let again = workspace::scaffold(&app.workspace).expect("scaffolded again");
    assert!(again.created.is_empty(), "{:?}", again.created);
}

/// PLAN 7.11: the convention is laid down *and* the folder is versioned, in
/// one press. Nothing is committed by it.
///
/// Folded as `workspace_scaffold` folds them.
#[tokio::test]
async fn scaffolding_leaves_a_repository_with_no_commits_in_it() {
    let app = App::new();

    let report = workspace::scaffold(&app.workspace)
        .expect("scaffolded")
        .versioned(aegis_lib::git::ensure(&app.workspace, None).await);

    // A machine with no `git` takes the other row of the table: the
    // directories are the job, they were done, and the report says the rest
    // did not happen. Everything below is about the row this machine is on.
    let Some(problem) = report.problem else {
        assert!(report.initialized, "{report:?}");
        assert!(app.workspace.join(".git").is_dir(), "there is a repository");
        assert!(
            !app.workspace.join(".git/index").exists(),
            "nothing was staged"
        );
        assert_eq!(
            std::fs::read_dir(app.workspace.join(".git/refs/heads"))
                .map(Iterator::count)
                .unwrap_or(0),
            0,
            "nothing was committed"
        );
        assert!(!app.workspace.join(".gitignore").exists());
        return;
    };

    assert!(!report.initialized, "{problem}");
    assert!(
        workspace::layout(&app.workspace).complete,
        "the directories were laid down anyway"
    );
}

// ---------------------------------------------------------------------------
// 2. The read path
// ---------------------------------------------------------------------------

/// What is in the files is in the next request. This is `COS.md` *read* —
/// "retrieve at session start" — and it is what a session gets instead of
/// re-reading a transcript.
#[test]
fn the_next_request_carries_what_the_files_say() {
    let app = App::new();
    workspace::scaffold(&app.workspace).expect("scaffolded");
    std::fs::write(
        app.workspace.join(STATUS_FILE),
        "# Status\n\n## Blocked\n\nthe staging deploy needs a key\n",
    )
    .expect("write");

    let prompt = app.next_system_message();

    assert!(
        prompt.contains("the staging deploy needs a key"),
        "the board reaches the model: {prompt}"
    );
    assert!(
        prompt.contains("not in this conversation"),
        "and the write rule with it: {prompt}"
    );
}

// ---------------------------------------------------------------------------
// 3. The write path — no new agent type, no new tool
// ---------------------------------------------------------------------------

/// The exit condition: a decision is filed with the tools that already exist,
/// it is gated, it survives the turn, and the next request knows about it.
#[tokio::test]
async fn a_decision_is_filed_with_fs_write_and_comes_back_in_the_next_request() {
    let app = App::new();
    workspace::scaffold(&app.workspace).expect("scaffolded");
    app.say("record that we are keeping the tray");

    let sink = Recorder::default();
    let filed = "# Decisions\n\n## 2026-08-30 — keep the tray\n\
                 Decision:  the app stays in the tray when the window closes\n\
                 Because:   a scheduler needs the process alive\n";
    let reason = app.write_turn(&sink, DECISIONS_FILE, filed).await;

    assert_eq!(reason, StopReason::Stop);

    // The gate was not bypassed on the way: writing a shared file is an
    // ordinary mutating call, and Phase 11 does not get to make it special.
    let names = sink.names();
    assert!(
        names.contains(&"tool:approval_required"),
        "the write was put to the user: {names:?}"
    );

    assert!(
        app.read(DECISIONS_FILE).contains("keep the tray"),
        "the decision is on disk"
    );
    assert!(
        app.next_system_message().contains("keep the tray"),
        "and in the request the next turn will send"
    );

    // And the write did not become a commit (PLAN 7.11). Approving an
    // `fs_write` is approval to write that file, not to put it on a branch,
    // and there is no path in the runtime that stages or commits anything.
    let heads = app.workspace.join(".git/refs/heads");
    assert_eq!(
        std::fs::read_dir(&heads).map(Iterator::count).unwrap_or(0),
        0,
        "a gated write leaves the history exactly where it was"
    );
    assert!(
        !app.workspace.join(".git/index").exists(),
        "and stages nothing"
    );
}

/// The same turn, in a workspace that never opted in: the write still works —
/// `decisions/` is an ordinary path — but nothing is read back into the prompt,
/// because there is no convention there to read.
#[tokio::test]
async fn a_write_outside_the_convention_is_still_just_a_write() {
    let app = App::new();
    app.say("put something in a file");

    let sink = Recorder::default();
    let reason = app.write_turn(&sink, "notes.txt", "some text").await;

    assert_eq!(reason, StopReason::Stop);
    assert_eq!(app.read("notes.txt"), "some text");
    assert_eq!(workspace::digest(&app.workspace), None);
}
