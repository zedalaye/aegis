//! The world — a workspace's constitution — through the crate's public
//! surface (`PLAN.md` § 7.2; `COS.md` *Work*).
//!
//! The unit tests inside `world.rs` cover the reader, the `sources.yml` parser
//! and the drift measurement; the ones inside `policy/mod.rs` cover the rows the
//! gate gained. This file covers what neither of those can: that the runtime
//! *as a whole* behaves the way the missed half of Phase 11 says it must, with
//! no new tool, no new write path and no new agent type.
//!
//! Four claims, in the order they matter:
//!
//! 1. It is opt-in. A workspace nobody founded a world in is judged, prompted
//!    and read exactly as it was before this slice.
//! 2. The frame reaches the model, and the essence does not. What is injected is
//!    a constraint and a status — a few lines — never the constitution itself.
//! 3. A specialist cannot amend the world. Not an approval it could talk its way
//!    through: a refusal, delivered as an ordinary `E_DENIED` result, with no
//!    dialog raised and nothing written.
//! 4. A source that has already been perceived cannot be reopened, and a source
//!    that has moved can. That pair is the whole economics of the thing: the
//!    round-trip is paid once, and the delta is the one legitimate re-read.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::json;
use tempfile::TempDir;

use aegis_lib::agent::event::EventSink;
use aegis_lib::agent::transcript;
use aegis_lib::agent::turn::{self, TurnPlan};
use aegis_lib::agent::wire::{ModelEvent, StopReason, WireMessage};
use aegis_lib::{
    Agent, ApprovalRegistry, AuditLog, Event, FakeProvider, GrantStore, MemoryStore, Message,
    SessionState, SessionStore, SourceState, Standing, Turn, TurnRegistry, DEFAULT_AGENT_ID,
};

/// Collects every event a turn emits, so a test can say what was *not* raised.
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

/// A data directory, a workspace, and every store a turn borrows.
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

    /// Writes `body` at `rel` inside the workspace, creating what it has to.
    fn put(&self, rel: &str, body: &str) -> PathBuf {
        let path = self.workspace.join(rel);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
        std::fs::write(&path, body).expect("write");
        path
    }

    /// Founds a world with one declared source, already perceived.
    ///
    /// The digest is computed from the file rather than written by hand, which
    /// is what a person amending `world/sources.yml` would do — and what the
    /// `world.perceive-delta` runbook proposes.
    fn found_a_world(&self) -> PathBuf {
        self.put(
            "world/essence.md",
            "# Essence\n\nA ledger of one household's standing orders.\n",
        );
        let dump = self.put("sources/legacy.sql", "select 1;\n");
        let digest = sha256(&dump);
        self.put(
            "world/sources.yml",
            &format!(
                "# what this world was perceived from\n\
                 sources:\n\
                 \x20 - path: sources/legacy.sql\n\
                 \x20   sha256: {digest}\n"
            ),
        );
        dump
    }

    /// The system message the *next* request would carry, assembled the way the
    /// turn loop assembles it.
    ///
    /// `delegated` is the fact the frame turns on: the same world says different
    /// things to a brief and to a session somebody is sitting in, because the
    /// gate does different things to their writes.
    fn next_system_message(&self, delegated: bool) -> String {
        let history = self.sessions.messages(&self.session_id).expect("messages");
        let shared = aegis_lib::workspace::digest(&self.workspace);
        let world = aegis_lib::world::block(&self.workspace, delegated);
        let agent = Agent::builtin();
        let request = transcript::build(
            "m",
            &transcript::Context {
                agent: &agent,
                workspace: Some(&self.workspace),
                memories: None,
                skills: None,
                world: world.as_deref(),
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

    /// Runs one scripted turn making one tool call, and returns its envelope.
    ///
    /// Nothing answers an approval here on purpose: every call these tests make
    /// is one the gate is supposed to settle without a person. A call that did
    /// raise a dialog would hang, which is why the recorder is checked for one.
    async fn one_call(
        &self,
        sink: &Recorder,
        delegated: bool,
        tool: &str,
        args: serde_json::Value,
    ) -> serde_json::Value {
        let turn_id = "turn-1";
        let cancel = self.turns.begin(&self.session_id, turn_id).expect("free");

        let provider = FakeProvider::scripted(vec![vec![
            ModelEvent::ToolCallDelta {
                index: 0,
                id: Some("call_1".to_owned()),
                name: Some(tool.to_owned()),
                args_delta: args.to_string(),
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
        };
        // A brief is what makes a run a specialist's, and it is a fact about the
        // *run* rather than about the identity: the same built-in assistant is
        // the cabinet in one session and a specialist in the next.
        let open = aegis_lib::handoff::Open::new("handoff-1");
        let reason = Turn {
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
            standing: if delegated {
                Standing::Delegated(&open)
            } else {
                Standing::Own(None)
            },
            unattended: None,
        }
        .run(&plan, &cancel)
        .await;
        self.turns
            .finish(&self.session_id, turn_id, turn::resting_state(reason));

        self.sessions
            .messages(&self.session_id)
            .expect("messages")
            .iter()
            .filter(|message| message.tool_call_id.is_some())
            .map(|message| serde_json::from_str(&message.text).expect("an envelope"))
            .next_back()
            .expect("the call was answered")
    }

    fn say(&self, text: &str) {
        self.sessions
            .append(&self.session_id, Message::user(text), SessionState::Running)
            .expect("the user's message is stored");
    }
}

/// A file's SHA-256, lower-case hex — what `world/sources.yml` records.
fn sha256(path: &Path) -> String {
    use sha2::Digest as _;
    let bytes = std::fs::read(path).expect("read");
    format!("{:x}", sha2::Sha256::digest(&bytes))
}

// ---------------------------------------------------------------------------
// 1. Opt-in
// ---------------------------------------------------------------------------

/// A workspace nobody founded a world in is exactly the workspace it was.
///
/// This is the claim PLAN 7.2 makes about theatre: five empty templates in a
/// watch folder or a wish list are worse than nothing, so nothing here creates
/// them, nothing nags about them, and no read is refused on the strength of a
/// constitution that does not exist.
#[tokio::test]
async fn a_workspace_without_a_world_is_untouched_by_one() {
    let app = App::new();
    app.put("sources/legacy.sql", "select 1;\n");
    app.say("what is this project?");

    let status = aegis_lib::world::status(&app.workspace);
    assert!(!status.present);
    assert!(!status.drifted);
    assert!(status.sources.is_empty());
    assert!(
        status.files.iter().all(|file| !file.exists),
        "the panel can still say what a world is without pretending there is one"
    );

    let prompt = app.next_system_message(false);
    assert!(!prompt.contains("world"), "{prompt}");

    // And a dump nobody declared is an ordinary file in an ordinary folder.
    let sink = Recorder::default();
    let result = app
        .one_call(
            &sink,
            false,
            "fs_read",
            json!({ "path": "sources/legacy.sql" }),
        )
        .await;
    assert_eq!(result["ok"], true, "{result:#}");
    assert!(
        result["content"]
            .as_str()
            .is_some_and(|c| c.contains("select 1;")),
        "{result:#}"
    );
}

// ---------------------------------------------------------------------------
// 2. The frame reaches the model; the essence does not
// ---------------------------------------------------------------------------

/// What is injected is a *constraint plus a status*, and it is a harness
/// injection rather than a skill because a skill can be skipped.
///
/// The negative half is the one that costs: `essence.md` stays on disk. The
/// system message is a policy summary plus what is true right now (PLAN 7.1),
/// and a constitution pasted into it would be paid for on every turn of every
/// session for as long as the world lives.
#[test]
fn the_frame_is_injected_and_the_constitution_stays_on_disk() {
    let app = App::new();
    app.found_a_world();
    app.say("draft the standing orders screen");

    let prompt = app.next_system_message(false);

    assert!(prompt.contains("écart"), "{prompt}");
    assert!(
        prompt.contains("`essence.md`"),
        "the file is named: {prompt}"
    );
    assert!(
        prompt.contains("sources/legacy.sql"),
        "and so is what it must not reopen: {prompt}"
    );
    assert!(
        !prompt.contains("standing orders."),
        "the essence itself is never pasted: {prompt}"
    );

    // The status is the drift the block can see for free. Nothing has moved, so
    // it says nothing — silence is the correct output of a pass with nothing to
    // report, and a block that claimed "in step" would be claiming more than it
    // measured.
    assert!(!prompt.contains("have moved"), "{prompt}");
}

/// The frame says different things to a brief and to a session, because the
/// gate *does* different things to their writes — and a prompt that refused
/// what the gate would have asked about is the worse of the two errors.
///
/// This is a regression: the first version of the frame told every session "you
/// do not write `world/`", so a person asking for help founding one got a model
/// that declined and never reached the dialog that would have said yes. The
/// audit log for that failure has no `fs_write` in it at all, which is the shape
/// of a prompt refusing rather than a gate refusing.
#[test]
fn the_frame_forbids_a_brief_and_invites_a_session() {
    let app = App::new();
    app.found_a_world();
    app.say("help me write the essence");

    let brief = app.next_system_message(true);
    assert!(brief.contains("You do not write `world/`"), "{brief}");
    assert!(brief.contains("needs_you"), "{brief}");
    assert!(
        !brief.contains("world.draft"),
        "a brief is not pointed at the runbook it may not run: {brief}"
    );

    let session = app.next_system_message(false);
    assert!(
        !session.contains("You do not write `world/`"),
        "the flat refusal is the bug this test exists for: {session}"
    );
    assert!(
        session.contains("world.draft"),
        "and the session is told what to use: {session}"
    );
    assert!(
        session.contains("approval"),
        "and that each write is put to the operator: {session}"
    );

    // What both are told is the same, and it is the part that is about reading.
    for prompt in [&brief, &session] {
        assert!(prompt.contains("écart"), "{prompt}");
        assert!(prompt.contains("sources/legacy.sql"), "{prompt}");
    }
}

/// A source the operator has replaced shows up as an attention item on the very
/// next request, without anything having to be re-measured by hand.
#[test]
fn a_source_that_moved_is_an_attention_item_in_the_next_prompt() {
    let app = App::new();
    app.found_a_world();
    app.put("sources/legacy.sql", "select 1;\nselect 2;\n");

    let prompt = app.next_system_message(false);
    assert!(prompt.contains("have moved"), "{prompt}");
    assert!(prompt.contains("sources/legacy.sql"), "{prompt}");

    let status = aegis_lib::world::status(&app.workspace);
    assert!(status.drifted);
    assert_eq!(status.sources[0].state, SourceState::Drifted);
}

// ---------------------------------------------------------------------------
// 3. A specialist reads the world and does not write it
// ---------------------------------------------------------------------------

/// `COS.md` *Work*: not an ask with a session grant — a refusal.
///
/// Three things are asserted, and the second and third are the ones that make
/// it a policy rather than a warning: no dialog was raised, so there was nothing
/// for anybody to click through; and the file on disk is byte for byte what it
/// was.
#[tokio::test]
async fn a_brief_cannot_amend_the_world_and_is_told_what_to_do_instead() {
    let app = App::new();
    app.found_a_world();
    app.say("rewrite the essence so this fits");

    let before =
        std::fs::read_to_string(app.workspace.join("world/essence.md")).expect("the essence");
    let sink = Recorder::default();
    let result = app
        .one_call(
            &sink,
            true,
            "fs_write",
            json!({ "path": "world/essence.md", "content": "# Essence\n\nWhatever suits.\n" }),
        )
        .await;

    assert_eq!(result["ok"], false, "{result:#}");
    assert_eq!(result["error"]["code"], "E_DENIED", "{result:#}");
    let message = result["error"]["message"].as_str().expect("a reason");
    assert!(message.contains("needs_you"), "{message}");

    assert!(
        !sink.names().contains(&"approval_requested"),
        "a refusal opens no dialog: {:?}",
        sink.names()
    );
    assert_eq!(
        std::fs::read_to_string(app.workspace.join("world/essence.md")).expect("the essence"),
        before,
        "and nothing was written"
    );
}

/// The same call outside a brief is a cabinet act: it stops and asks.
///
/// Amending the world is a human decision (`COS.md` *Work*), which in this
/// harness means the one thing a dialog is for — and the dialog offers a
/// standing approval of its own, because founding a world is six files and six
/// identical High-risk prompts in a row is how somebody is taught to stop
/// reading them. What that approval covers is the constitution and nothing
/// else; `policy` has the tests for the two directions of that.
#[tokio::test]
async fn amending_the_world_from_the_cabinet_stops_and_asks() {
    let app = App::new();
    app.found_a_world();
    app.say("record that we are dropping the second ledger");

    let sink = Recorder::default();
    let call = app.one_call(
        &sink,
        false,
        "fs_write",
        json!({ "path": "world/decisions.md", "content": "## Dropped the second ledger\n" }),
    );

    // Nothing answers it. The turn parks on the dialog, which is the behaviour
    // under test, so the request is read and then the turn is stopped.
    let watching = async {
        for _ in 0..1000 {
            if let Some(request) = app.approvals.list(Some(&app.session_id)).into_iter().next() {
                app.turns
                    .cancel(&app.session_id, &request.turn_id)
                    .expect("the turn is still running");
                return request;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        panic!("no approval was raised");
    };

    let (_, request) = tokio::join!(call, watching);
    assert_eq!(request.tool, "fs_write");
    assert_eq!(request.title, "Amend the world");
    assert!(
        request.session_grant_allowed,
        "a person may sign once for the rest of the files: {request:?}"
    );
    assert!(
        request.scope_label.contains("world/"),
        "and what they sign for is the constitution, not the workspace: {}",
        request.scope_label
    );
    assert!(
        request.reason.contains("constitution"),
        "and the dialog says why: {}",
        request.reason
    );
}

// ---------------------------------------------------------------------------
// 4. Perceived once, re-read only for the delta
// ---------------------------------------------------------------------------

/// The economics of the whole slice, as two calls.
///
/// A declared source that still hashes to what the world recorded is *refused*,
/// not asked about: what it said is in `world/`, and re-reading it is the
/// round-trip the world exists to have paid once. The moment the operator drops
/// a new one it is an ordinary contained read again, because perceiving that
/// delta is the only legitimate re-perception there is.
#[tokio::test]
async fn a_perceived_source_is_refused_and_the_delta_reads() {
    let app = App::new();
    app.found_a_world();
    app.say("read the dump and tell me what this project is");

    let sink = Recorder::default();
    let refused = app
        .one_call(
            &sink,
            false,
            "fs_read",
            json!({ "path": "sources/legacy.sql" }),
        )
        .await;

    assert_eq!(refused["ok"], false, "{refused:#}");
    assert_eq!(refused["error"]["code"], "E_DENIED", "{refused:#}");
    let message = refused["error"]["message"].as_str().expect("a reason");
    assert!(message.contains("world/"), "{message}");
    assert!(
        !sink.names().contains(&"approval_requested"),
        "denied, not asked: {:?}",
        sink.names()
    );

    // The operator drops a new dump. The bounded re-perception can now happen,
    // through the same tool and with no grant, exception or new command.
    app.put("sources/legacy.sql", "select 1;\nselect 2;\n");
    let sink = Recorder::default();
    let read = app
        .one_call(
            &sink,
            false,
            "fs_read",
            json!({ "path": "sources/legacy.sql" }),
        )
        .await;

    assert_eq!(read["ok"], true, "{read:#}");
    assert!(
        read["content"]
            .as_str()
            .is_some_and(|c| c.contains("select 2;")),
        "{read:#}"
    );
}

/// Drift stops a brief before it launches — except the one that is about the
/// delta, which names it in its inputs.
///
/// `PLAN.md` § 7.2: route a bounded perceive-delta *or* the work, never both.
/// Compiling on top of a source nobody has re-read is an instance built from a
/// schema that is already known to be wrong.
#[test]
fn drift_holds_the_briefs_that_are_not_about_it() {
    let app = App::new();
    app.found_a_world();

    assert!(
        aegis_lib::world::blocking(&app.workspace, &[".aegis/briefs/screen.md".to_owned()])
            .is_none(),
        "nothing has moved, so nothing is held"
    );

    app.put("sources/legacy.sql", "select 1;\nselect 2;\n");

    let held = aegis_lib::world::blocking(&app.workspace, &[".aegis/briefs/screen.md".to_owned()])
        .expect("the brief is held");
    assert!(held.contains("sources/legacy.sql"), "{held}");
    assert!(held.contains("perceive-delta"), "{held}");

    assert!(
        aegis_lib::world::blocking(&app.workspace, &["sources/legacy.sql".to_owned()]).is_none(),
        "the delta itself goes out"
    );
}
