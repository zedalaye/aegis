//! Per-agent memory and compaction, through the crate's public surface.
//!
//! Phase 14's exit condition (PLAN 7.3):
//!
//! 1. An approved memory reaches the next system message; a refused one does not.
//! 2. Memories do not leak across identities.
//! 3. A forced compaction keeps goal, blockers and the decisions path, and
//!    leaves the transcript on disk untouched.
//! 4. Memory and the workspace digest are still in the request after the fold.
//!
//! And an identity with nothing learned sends the Phase 13 request. Commands
//! need a Tauri app; everything below them runs here on temp files.

use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::json;
use tempfile::TempDir;

use aegis_lib::agent::event::EventSink;
use aegis_lib::agent::transcript;
use aegis_lib::agent::turn::TurnPlan;
use aegis_lib::agent::wire::{ModelEvent, StopReason, WireMessage};
use aegis_lib::policy::tool;
use aegis_lib::store::memories;
use aegis_lib::Standing;
use aegis_lib::{
    compact, Agent, AgentDraft, AgentStore, ApprovalDecision, ApprovalRegistry, AuditLog, Event,
    FakeProvider, GrantStore, MemoryDraft, MemoryKind, MemoryStore, Message, SessionState,
    SessionStore, ToolCallRecord, ToolCallStatus, Turn, TurnRegistry, DEFAULT_PROVIDER_ID,
};

/// Collects every event a turn emits.
#[derive(Debug, Default)]
struct Recorder {
    events: Mutex<Vec<Event>>,
}

impl EventSink for Recorder {
    fn emit(&self, event: Event) {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(event);
    }
}

/// A data directory, a scaffolded workspace, and every store the runtime holds.
struct App {
    _dir: TempDir,
    workspace: PathBuf,
    library: PathBuf,
    captures: PathBuf,
    agents: AgentStore,
    sessions: SessionStore,
    memories: MemoryStore,
    turns: TurnRegistry,
    grants: GrantStore,
    approvals: ApprovalRegistry,
    audit: AuditLog,
}

impl App {
    fn new() -> Self {
        let dir = TempDir::new().expect("temp dir");
        let data = dir.path().join("data");
        let workspace = dir.path().join("work");
        std::fs::create_dir_all(&data).expect("data dir");
        std::fs::create_dir_all(&workspace).expect("workspace dir");

        let workspace = dunce::canonicalize(&workspace).expect("canonical workspace");
        // The convention laid down the way the sidebar button lays it down, so
        // the digest these tests assert on is the one a user would get.
        aegis_lib::workspace::scaffold(&workspace).expect("the convention is laid down");

        Self {
            library: data.join("skills"),
            captures: data.join("captures"),
            agents: AgentStore::load(&data),
            sessions: SessionStore::load(&data),
            memories: MemoryStore::load(&data),
            audit: AuditLog::new(&data),
            workspace,
            _dir: dir,
            turns: TurnRegistry::new(),
            grants: GrantStore::new(),
            approvals: ApprovalRegistry::new(),
        }
    }

    /// An identity granted the memory tools.
    fn scribe(&self, name: &str) -> Agent {
        self.agents
            .create(&AgentDraft {
                name: name.to_owned(),
                role: "keeps notes and remembers what matters".to_owned(),
                instructions: String::new(),
                provider_id: DEFAULT_PROVIDER_ID.to_owned(),
                tools: vec![
                    tool::FS_READ.to_owned(),
                    tool::FS_WRITE.to_owned(),
                    tool::MEMORY_WRITE.to_owned(),
                    tool::MEMORY_SEARCH.to_owned(),
                ],
                skills: Vec::new(),
                runs_per_day: 24,
            })
            .expect("the identity is accepted")
    }

    fn session_as(&self, agent: &Agent) -> String {
        self.sessions
            .create("project-1", None, &agent.id)
            .expect("session")
            .id
    }

    fn say(&self, session_id: &str, text: &str) {
        self.sessions
            .append(session_id, Message::user(text), SessionState::Running)
            .expect("the user's message is stored");
    }

    /// The request the *next* turn of `session_id` would send, assembled the
    /// way the turn loop assembles it — fold and all.
    fn next_request(&self, session_id: &str, agent: &Agent) -> aegis_lib::ModelRequest {
        let (history, compaction) = self.sessions.context(session_id).expect("context");
        let raw = compact::tail(
            &history,
            compaction
                .as_ref()
                .map(|held| held.through_message_id.as_str()),
        );
        let held = self.memories.list_for(&agent.id);
        let block = memories::prompt_block(&held, held.len());
        let shared = aegis_lib::workspace::digest(&self.workspace);

        transcript::build(
            "m",
            &transcript::Context {
                agent,
                workspace: Some(&self.workspace),
                exec_host: None,
                memories: block.as_deref(),
                skills: None,
                world: None,
                shared: shared.as_deref(),
                compacted: compaction.as_ref().map(|held| held.state.as_str()),
                unattended: false,
            },
            raw,
            aegis_lib::tools::schemas_for(&agent.tools, &aegis_lib::ConnectorCatalog::empty()),
        )
    }

    /// The system message that request would carry.
    fn next_system_message(&self, session_id: &str, agent: &Agent) -> String {
        match self.next_request(session_id, agent).messages.first() {
            Some(WireMessage::System { content }) => content.clone(),
            other => panic!("the first message is not a system message: {other:?}"),
        }
    }

    /// Every user and assistant message body the next request would carry,
    /// concatenated — what the model would actually re-read.
    fn next_conversation(&self, session_id: &str, agent: &Agent) -> String {
        self.next_request(session_id, agent)
            .messages
            .iter()
            .filter_map(|message| match message {
                WireMessage::User { content } => Some(content.clone()),
                WireMessage::Assistant { content, .. } => content.clone(),
                _ => None,
            })
            .collect::<Vec<String>>()
            .join("\n")
    }

    /// Waits for the next approval the turn raises, and answers it.
    ///
    /// Polled while the turn runs concurrently, as in `tests/approvals.rs`.
    async fn answer_next(&self, session_id: &str, decision: ApprovalDecision) {
        for _ in 0..500 {
            if let Some(request) = self.approvals.list(Some(session_id)).into_iter().next() {
                self.approvals
                    .resolve(&request.request_id, decision, &self.grants)
                    .expect("the request is open");
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        panic!("no approval was raised");
    }

    /// Runs one turn whose rounds are scripted, one tool call per round.
    ///
    /// `answers` is what a click on the approval dialog would be, in order, for
    /// each prompt the turn raises. A round that asks nothing consumes none.
    async fn scripted_turn(
        &self,
        session_id: &str,
        agent: &Agent,
        rounds: Vec<(&str, serde_json::Value)>,
        answers: &[ApprovalDecision],
    ) -> StopReason {
        let turn_id = "turn-1";
        let cancel = self.turns.begin(session_id, turn_id).expect("free");

        let script = rounds
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
            session_id: session_id.to_owned(),
            turn_id: turn_id.to_owned(),
            workspace: Some(self.workspace.clone()),
            exec_host: None,
        };
        let sink = Recorder::default();

        let turn = Turn {
            agent,
            sessions: &self.sessions,
            turns: &self.turns,
            grants: &self.grants,
            approvals: &self.approvals,
            audit: &self.audit,
            provider: &provider,
            sink: &sink,
            self_exe: None,
            captures: &self.captures,
            skills: &self.library,
            memories: &self.memories,
            connectors: aegis_lib::Connectors::none(),
            standing: Standing::Own(None),
            unattended: None,
        };
        let running = turn.run(&plan, &cancel);

        let answering = async {
            for decision in answers {
                self.answer_next(session_id, *decision).await;
            }
        };
        let (reason, ()) = tokio::join!(running, answering);

        self.turns.finish(session_id, turn_id, SessionState::Idle);
        reason
    }

    /// The envelopes the model was handed in `session_id`, oldest first.
    fn envelopes(&self, session_id: &str) -> Vec<serde_json::Value> {
        self.sessions
            .messages(session_id)
            .expect("messages")
            .iter()
            .filter(|message| message.tool_call_id.is_some())
            .map(|message| serde_json::from_str(&message.text).expect("an envelope"))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// 1. A memory goes end-to-end
// ---------------------------------------------------------------------------

/// The whole loop: the model asks, the user is asked, the record lands, and the
/// next turn carries it. The middle step is the one worth having a test for —
/// a memory reaches every later turn's instructions, so it goes through the
/// gate like a write.
#[tokio::test]
async fn a_remembered_thing_is_approved_stored_and_carried_into_the_next_turn() {
    let app = App::new();
    let scribe = app.scribe("Scribe");
    let session = app.session_as(&scribe);
    app.say(&session, "the client only reads French");

    let before = app.next_system_message(&session, &scribe);
    assert!(
        !before.contains("What you have learned"),
        "an identity that has learned nothing carries no block: {before}"
    );

    app.scripted_turn(
        &session,
        &scribe,
        vec![(
            tool::MEMORY_WRITE,
            json!({
                "kind": "preference",
                "text": "this client wants everything in French",
                "source": ".aegis/briefs/README.md",
            }),
        )],
        &[ApprovalDecision::AllowOnce],
    )
    .await;

    let held = app.memories.list_for(&scribe.id);
    assert_eq!(held.len(), 1, "one memory was recorded");
    assert_eq!(held[0].kind, MemoryKind::Preference);
    assert_eq!(held[0].source.as_deref(), Some(".aegis/briefs/README.md"));

    let after = app.next_system_message(&session, &scribe);
    assert!(after.contains("wants everything in French"), "{after}");
    assert!(
        after.contains(".aegis/briefs/README.md"),
        "the citation too"
    );
    assert!(
        after.contains("the user will correct it"),
        "the model is told who owns a correction: {after}"
    );
}

/// A denial is an ordinary result, and it records nothing. The turn carries on
/// — the model reads the refusal and can say so.
#[tokio::test]
async fn a_refused_memory_is_not_remembered() {
    let app = App::new();
    let scribe = app.scribe("Scribe");
    let session = app.session_as(&scribe);
    app.say(&session, "remember something");

    app.scripted_turn(
        &session,
        &scribe,
        vec![(
            tool::MEMORY_WRITE,
            json!({ "kind": "convention", "text": "deploy on Fridays" }),
        )],
        &[ApprovalDecision::Deny],
    )
    .await;

    assert_eq!(app.memories.count_for(&scribe.id), 0, "nothing was stored");

    let envelope = app.envelopes(&session).pop().expect("an answer");
    assert_eq!(envelope["ok"], json!(false));
    assert_eq!(envelope["error"]["code"], "E_DENIED");

    let prompt = app.next_system_message(&session, &scribe);
    assert!(!prompt.contains("deploy on Fridays"), "{prompt}");
}

/// The consolidate half of `COS.md` *Memory*, from the outside: an identity
/// told the same thing twice holds it once, and is told that it already knew.
#[tokio::test]
async fn being_told_the_same_thing_twice_holds_it_once() {
    let app = App::new();
    let scribe = app.scribe("Scribe");
    let session = app.session_as(&scribe);
    app.say(&session, "remember it, twice");

    let write = (
        tool::MEMORY_WRITE,
        json!({ "kind": "preference", "text": "answers in French" }),
    );
    app.scripted_turn(
        &session,
        &scribe,
        vec![write.clone(), write],
        &[ApprovalDecision::AllowSession],
    )
    .await;

    assert_eq!(app.memories.count_for(&scribe.id), 1);

    let envelopes = app.envelopes(&session);
    assert_eq!(envelopes[0]["meta"]["new"], json!(true));
    assert_eq!(
        envelopes[1]["meta"]["new"],
        json!(false),
        "the second write says it was already held"
    );
}

// ---------------------------------------------------------------------------
// 2. A memory belongs to one identity
// ---------------------------------------------------------------------------

/// `COS.md` *Loop* wants a role clonable without its rotten memory. That is
/// only true if memory is scoped, so this asserts it from both directions the
/// model can reach: the prompt, and the search.
#[tokio::test]
async fn one_identitys_memory_never_reaches_another() {
    let app = App::new();
    let scribe = app.scribe("Scribe");
    let reviewer = app.scribe("Reviewer");

    let theirs = app.session_as(&scribe);
    app.say(&theirs, "remember this");
    app.scripted_turn(
        &theirs,
        &scribe,
        vec![(
            tool::MEMORY_WRITE,
            json!({ "kind": "exception", "text": "never touch the vendored crate" }),
        )],
        &[ApprovalDecision::AllowOnce],
    )
    .await;

    // The other identity's standing context.
    let mine = app.session_as(&reviewer);
    let prompt = app.next_system_message(&mine, &reviewer);
    assert!(
        !prompt.contains("vendored crate"),
        "another identity's memory is not standing context: {prompt}"
    );

    // And the other identity's search.
    app.say(&mine, "what do you know about the vendored crate");
    app.scripted_turn(
        &mine,
        &reviewer,
        vec![(tool::MEMORY_SEARCH, json!({ "query": "vendored" }))],
        &[],
    )
    .await;

    let envelope = app.envelopes(&mine).pop().expect("an answer");
    assert_eq!(envelope["ok"], json!(true), "a miss is not a failure");
    assert_eq!(envelope["meta"]["matched"], json!(0));
    assert_eq!(envelope["meta"]["held"], json!(0));
}

/// Deleting an identity takes its memories with it. They are unreachable
/// otherwise — every accessor takes an identity — and a document that kept them
/// would grow forever with records nothing can name.
#[test]
fn deleting_an_identity_forgets_what_it_knew() {
    let app = App::new();
    let scribe = app.scribe("Scribe");

    app.memories
        .save(
            &scribe.id,
            None,
            &MemoryDraft {
                kind: MemoryKind::Convention,
                text: "releases are tagged first".to_owned(),
                source: None,
            },
        )
        .expect("recorded");

    app.agents
        .delete(&scribe.id)
        .expect("no session runs as it");
    app.memories
        .forget_for_agent(&scribe.id)
        .expect("its memories go with it");

    assert_eq!(app.memories.count_for(&scribe.id), 0);
}

// ---------------------------------------------------------------------------
// 3. and 4. Compaction, and what survives it
// ---------------------------------------------------------------------------

/// A session long enough to fold: a goal, a decision filed, a blocked run, and
/// four later turns that stay raw.
fn long_session(app: &App, agent: &Agent) -> String {
    let session = app.session_as(agent);

    app.say(&session, "get the staging deploy working again");
    app.sessions
        .append(
            &session,
            Message::assistant(
                "Filing what we settled.",
                vec![
                    ToolCallRecord {
                        call_id: "c1".to_owned(),
                        tool: tool::FS_WRITE.to_owned(),
                        args_json: json!({
                            "path": ".aegis/decisions/DECISIONS.md",
                            "content": "roll back first",
                        })
                        .to_string(),
                        status: ToolCallStatus::Ok,
                        summary: None,
                        image_path: None,
                        thought_signature: None,
                    },
                    ToolCallRecord {
                        call_id: "c2".to_owned(),
                        tool: tool::SKILL_RETURN.to_owned(),
                        args_json: json!({
                            "status": "blocked",
                            "summary": "cannot reach the registry",
                            "open_questions": ["which registry does staging pull from"],
                        })
                        .to_string(),
                        status: ToolCallStatus::Ok,
                        summary: None,
                        image_path: None,
                        thought_signature: None,
                    },
                ],
            ),
            SessionState::Idle,
        )
        .expect("append");
    app.sessions
        .append(
            &session,
            Message::tool("c1", r#"{"ok":true}"#),
            SessionState::Idle,
        )
        .expect("append");
    app.sessions
        .append(
            &session,
            Message::tool("c2", r#"{"ok":true}"#),
            SessionState::Idle,
        )
        .expect("append");

    // A middle turn, whose detail is exactly what a fold is for losing.
    app.say(&session, "a long digression about the CI cache");
    app.sessions
        .append(
            &session,
            Message::assistant("A long answer about the CI cache.", Vec::new()),
            SessionState::Idle,
        )
        .expect("append");

    // Then enough recent turns that the ones above are the ones that fold.
    for n in 0..compact::KEEP_TURNS {
        app.say(&session, &format!("recent request {n}"));
        app.sessions
            .append(
                &session,
                Message::assistant(format!("recent reply {n}"), Vec::new()),
                SessionState::Idle,
            )
            .expect("append");
    }

    session
}

/// **The Phase 14 exit condition** (PLAN 7.3): after a forced compaction the
/// agent still knows the current goal, the open blockers and the path to
/// `DECISIONS.md`, and it does not replay the whole chat.
#[test]
fn after_a_forced_compaction_the_goal_the_blockers_and_the_ledger_survive() {
    let app = App::new();
    let scribe = app.scribe("Scribe");
    let session = long_session(&app, &scribe);

    let folded = app
        .sessions
        .compact(&session, true)
        .expect("compacting works")
        .expect("there is something to fold");
    assert!(folded.folded > 0);

    let prompt = app.next_system_message(&session, &scribe);

    // The goal.
    assert!(
        prompt.contains("Goal: get the staging deploy working again"),
        "{prompt}"
    );
    // The open blockers.
    assert!(
        prompt.contains("which registry does staging pull from"),
        "{prompt}"
    );
    // The path to the ledger — twice over, which is the point: the state names
    // it as a decision that was filed, and the Phase 11 digest names the file
    // itself on every request, fold or no fold.
    assert!(prompt.contains(".aegis/decisions/DECISIONS.md"), "{prompt}");
    assert!(
        prompt.contains("filed in .aegis/decisions/DECISIONS.md"),
        "the state says a decision was filed there: {prompt}"
    );

    // And it does not replay the whole chat.
    let conversation = app.next_conversation(&session, &scribe);
    assert!(
        !conversation.contains("a long digression about the CI cache"),
        "the folded turns are gone from the request: {conversation}"
    );
    assert!(
        conversation.contains("recent request 0"),
        "the recent turns are still raw: {conversation}"
    );
}

/// Nothing is deleted. The user is still reading the conversation they had, and
/// a fold that quietly truncated their transcript would be destroying the one
/// copy of it to save tokens.
#[test]
fn a_fold_changes_what_the_model_sees_and_not_what_is_on_disk() {
    let app = App::new();
    let scribe = app.scribe("Scribe");
    let session = long_session(&app, &scribe);

    let before = app.sessions.messages(&session).expect("messages").len();
    app.sessions
        .compact(&session, true)
        .expect("compacting works");
    let after = app.sessions.messages(&session).expect("messages");

    assert_eq!(after.len(), before, "every message is still there");
    assert!(
        after.iter().any(|message| message
            .text
            .contains("a long digression about the CI cache")),
        "including the ones that folded"
    );

    // And it survives a restart, pointer and all.
    let reopened = SessionStore::load(app._dir.path().join("data").as_path());
    let (messages, compaction) = reopened.context(&session).expect("context");
    assert_eq!(messages.len(), before);
    assert!(compaction.is_some(), "the fold is persisted");
}

/// Retrieve-after-compact (PLAN 7.3): what the identity *knows* is standing
/// context, so a fold cannot take it away. Same for the workspace digest.
#[test]
fn memory_and_the_workspace_digest_survive_a_fold() {
    let app = App::new();
    let scribe = app.scribe("Scribe");
    let session = long_session(&app, &scribe);

    app.memories
        .save(
            &scribe.id,
            None,
            &MemoryDraft {
                kind: MemoryKind::Exception,
                text: "staging never auto-deploys on a Friday".to_owned(),
                source: Some(".aegis/decisions/DECISIONS.md".to_owned()),
            },
        )
        .expect("recorded");

    app.sessions
        .compact(&session, true)
        .expect("compacting works");
    let prompt = app.next_system_message(&session, &scribe);

    assert!(
        prompt.contains("staging never auto-deploys on a Friday"),
        "memory is re-injected because it never folded: {prompt}"
    );
    assert!(
        prompt.contains(".aegis/status/STATUS.md"),
        "and so is the shared digest: {prompt}"
    );
}

/// A second fold supersedes the first rather than summarizing it. The state is
/// re-derived from the messages, which are all still there, so a session folded
/// five times is not a summary of a summary.
#[test]
fn folding_twice_re_derives_rather_than_compounding() {
    let app = App::new();
    let scribe = app.scribe("Scribe");
    let session = long_session(&app, &scribe);

    let first = app
        .sessions
        .compact(&session, true)
        .expect("compacting works")
        .expect("something folded");

    // Nothing has happened since, so there is nothing new to fold.
    assert!(
        app.sessions
            .compact(&session, true)
            .expect("compacting works")
            .is_none(),
        "a second fold with no new turns moves nothing"
    );

    // Two more turns, and the fold advances.
    for n in 0..2 {
        app.say(&session, &format!("later request {n}"));
        app.sessions
            .append(
                &session,
                Message::assistant(format!("later reply {n}"), Vec::new()),
                SessionState::Idle,
            )
            .expect("append");
    }

    let second = app
        .sessions
        .compact(&session, true)
        .expect("compacting works")
        .expect("something folded");

    assert_ne!(second.through_message_id, first.through_message_id);
    assert!(second.folded > first.folded);
    assert!(
        second
            .state
            .contains("Goal: get the staging deploy working again"),
        "the goal is re-derived from the transcript, not inherited: {}",
        second.state
    );
}

/// A short session is not a failure to compact. It is a session with nothing to
/// fold, and the panel draws that by finding no fold.
#[test]
fn a_short_session_folds_nothing_and_says_so() {
    let app = App::new();
    let scribe = app.scribe("Scribe");
    let session = app.session_as(&scribe);
    app.say(&session, "hello");

    assert!(app
        .sessions
        .compact(&session, true)
        .expect("compacting works")
        .is_none());

    let detail = app
        .sessions
        .open(&session, SessionState::Idle)
        .expect("open");
    assert!(detail.compaction.is_none());
}

// ---------------------------------------------------------------------------
// The other direction: this phase is a no-op for what came before it
// ---------------------------------------------------------------------------

/// An identity that has learned nothing, in a session too short to fold, sends
/// the request Phase 13 sent. A phase that silently changed every existing
/// session's prompt would be a migration nobody asked for.
#[test]
fn nothing_learned_and_nothing_folded_is_the_request_that_came_before() {
    let app = App::new();
    let scribe = app.scribe("Scribe");
    let session = app.session_as(&scribe);
    app.say(&session, "hello");

    let prompt = app.next_system_message(&session, &scribe);

    assert!(!prompt.contains("What you have learned"), "{prompt}");
    assert!(!prompt.contains("folded to state"), "{prompt}");
    // What Phase 11 and 12 put there is untouched.
    assert!(prompt.contains("Scribe"), "{prompt}");
    assert!(prompt.contains(".aegis/status/STATUS.md"), "{prompt}");
}

/// A session written before this phase carries no fold and reads whole. The
/// migration is `#[serde(default)]` and nothing else, and this is the assertion
/// that says so from outside the store.
#[test]
fn a_session_from_before_this_phase_reaches_the_model_whole() {
    let app = App::new();
    let scribe = app.scribe("Scribe");
    let session = long_session(&app, &scribe);

    let (messages, compaction) = app.sessions.context(&session).expect("context");
    assert!(compaction.is_none(), "nothing has folded it");
    assert_eq!(
        compact::tail(&messages, None).len(),
        messages.len(),
        "so all of it reaches the model"
    );
}
