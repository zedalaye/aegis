//! The handoff bus and the Chef-de-Cabinet loop, end to end.
//!
//! The unit tests inside `handoff/` cover the two objects and the bus's policy
//! — parallel, bounded, two attempts, then the human — against a scripted
//! runner. This file covers the Phase 15 exit condition of `PLAN.md` § 7.3,
//! which is a claim about the *whole* runtime:
//!
//! > one brief fans out to two specialists in parallel and the CoS returns a
//! > five-line status, not a concatenated transcript.
//!
//! Five claims, in the order they matter:
//!
//! 1. **Two briefs go out at once and come back as a board.** Two sessions
//!    open, under the two identities the briefs named, each runs its own turn,
//!    and what lands in the Chief of Staff's transcript is statuses and paths.
//! 2. **What comes back is not their transcripts.** The specialists say things
//!    in their own sessions. None of it reaches the CoS — structurally, because
//!    a report is the only thing a delegated run can produce.
//! 3. **A specialist works under its own perimeter.** The identity a brief
//!    names is the identity that runs, with the tools *it* holds — and it
//!    cannot re-delegate, because there are three roles and not four.
//! 4. **A run that does not answer escalates to the human, twice and no more.**
//!    Two attempts in the same session, then a `needs_you` on the board.
//! 5. **One run id covers the CoS and everyone under it.** Every audit line of
//!    every specialist carries the delegation the brief came from, beside the
//!    identity that made the call.
//!
//! The command layer above this needs a running Tauri application and is not
//! reachable from a test binary. Everything below it is — including
//! [`Delegating`], which is the production runner with its application-shaped
//! wiring lifted out into `HandoffHost`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::json;
use tempfile::TempDir;

use aegis_lib::agent::event::EventSink;
use aegis_lib::agent::wire::{ModelEvent, StopReason};
use aegis_lib::handoff::bus::{self, Runner};
use aegis_lib::handoff::{Brief, Priority, ReturnFormat};
use aegis_lib::policy::tool;
use aegis_lib::{
    Agent, AgentDraft, AgentStore, ApprovalRegistry, AuditEntry, AuditLog, Delegating, Event,
    FakeProvider, GrantStore, HandoffHost, HandoffPlan, MemoryStore, Message, Provider,
    SessionState, SessionStore, Standing, Turn, TurnPlan, TurnRegistry, DEFAULT_PROVIDER_ID,
};

/// Collects every event the runtime emits.
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
    turns: TurnRegistry,
    grants: GrantStore,
    approvals: ApprovalRegistry,
    audit: AuditLog,
    memories: MemoryStore,
    sink: Recorder,
}

impl App {
    fn new() -> Self {
        let dir = TempDir::new().expect("temp dir");
        let data = dir.path().join("data");
        let workspace = dir.path().join("work");
        std::fs::create_dir_all(&data).expect("data dir");
        std::fs::create_dir_all(&workspace).expect("workspace dir");
        let workspace = dunce::canonicalize(&workspace).expect("canonical workspace");

        // The convention the way the sidebar button lays it down, so `.aegis/briefs/`
        // is there and a delegation is filed the way a user would see it.
        aegis_lib::workspace::scaffold(&workspace).expect("the convention is laid down");

        let library = data.join(aegis_lib::skills::LIBRARY_DIR);
        aegis_lib::skills::seed(&library);

        Self {
            workspace,
            library,
            captures: data.join("captures"),
            agents: AgentStore::load(&data),
            sessions: SessionStore::load(&data),
            turns: TurnRegistry::new(),
            grants: GrantStore::new(),
            approvals: ApprovalRegistry::new(),
            audit: AuditLog::new(&data),
            memories: MemoryStore::load(&data),
            sink: Recorder::default(),
            _dir: dir,
        }
    }

    /// An identity with a narrow perimeter, as a specialist has.
    fn specialist(&self, name: &str, tools: Vec<String>) -> Agent {
        self.agents
            .create(&AgentDraft {
                name: name.to_owned(),
                role: format!("does the {name} part"),
                instructions: String::new(),
                provider_id: DEFAULT_PROVIDER_ID.to_owned(),
                tools,
                skills: Vec::new(),
                runs_per_day: 24,
            })
            .expect("the identity is accepted")
    }

    /// The Chief of Staff: routes, and holds the tool that lets it.
    fn chief(&self) -> Agent {
        self.specialist(
            "Chief",
            vec![
                tool::FS_READ.to_owned(),
                tool::FS_WRITE.to_owned(),
                tool::HANDOFF_DELEGATE.to_owned(),
            ],
        )
    }

    fn session_as(&self, agent: &Agent) -> String {
        self.sessions
            .create("project-1", None, &agent.id)
            .expect("session")
            .id
    }

    /// What a delegated run borrows, with a provider the test chooses.
    fn host<'a>(
        &'a self,
        provider: &'a (dyn Fn(&Agent) -> Box<dyn Provider> + Send + Sync),
    ) -> HandoffHost<'a> {
        HandoffHost {
            agents: &self.agents,
            sessions: &self.sessions,
            turns: &self.turns,
            grants: &self.grants,
            approvals: &self.approvals,
            audit: &self.audit,
            sink: &self.sink,
            self_exe: None,
            captures: &self.captures,
            skills: &self.library,
            memories: &self.memories,
            connectors: aegis_lib::Connectors::none(),
            provider,
        }
    }

    /// Every session of the project, oldest first.
    fn sessions(&self) -> Vec<aegis_lib::SessionSummary> {
        let mut rows = self.sessions.list("project-1", &|_| SessionState::Idle);
        rows.reverse();
        rows
    }

    /// Every audit line written so far, oldest first.
    fn audit_lines(&self) -> Vec<AuditEntry> {
        let mut lines = self.audit.tail(200, None).expect("tail");
        lines.reverse();
        lines
    }

    /// What a session's assistant said, joined.
    fn said_in(&self, session_id: &str) -> String {
        self.sessions
            .messages(session_id)
            .expect("messages")
            .iter()
            .filter(|message| message.role == aegis_lib::Role::Assistant)
            .map(|message| message.text.clone())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// The production runner, wired to the test's stores instead of to an
/// application.
///
/// This is [`Delegating`] — the same `file`, the same `attempt`, the same
/// sessions, turns and audit lines the shipping build uses. What is replaced is
/// only where the stores come from and which provider answers, which is exactly
/// the seam [`HandoffHost`] exists to be.
struct TestRunner {
    app: &'static App,
    inner: Delegating,
    /// What each delegated run does, keyed by the owner's name.
    script: Script,
}

/// One scripted turn per attempt, per owner.
type Script = std::collections::HashMap<String, Vec<Vec<ModelEvent>>>;

impl TestRunner {
    fn arc(app: &'static App, from_session_id: &str, script: Script) -> Arc<dyn Runner> {
        Arc::new(Self {
            app,
            inner: Delegating::new(
                "project-1".to_owned(),
                from_session_id.to_owned(),
                Some(app.workspace.clone()),
            ),
            script,
        })
    }
}

impl Runner for TestRunner {
    fn file(&self, slot: bus::Slot<'_>, brief: &Brief, rendered: &str) -> Option<String> {
        self.inner.file(slot, brief, rendered)
    }

    fn run<'a>(
        &'a self,
        slot: bus::Slot<'a>,
        brief: &'a Brief,
        filed: Option<&'a str>,
        cancel: &'a tokio_util::sync::CancellationToken,
    ) -> bus::Running<'a> {
        Box::pin(async move {
            let rounds = self.script.get(&brief.owner).cloned().unwrap_or_default();

            let provider = move |_: &Agent| -> Box<dyn Provider> {
                Box::new(FakeProvider::scripted(rounds.clone()))
            };
            let host = self.app.host(&provider);

            self.inner.attempt(&host, slot, brief, filed, cancel).await
        })
    }
}

/// Everything a `handoff_return` needs to be accepted.
fn returns(status: &str, summary: &str, said: &str) -> Vec<Vec<ModelEvent>> {
    vec![vec![
        ModelEvent::TextDelta {
            text: said.to_owned(),
        },
        ModelEvent::ToolCallDelta {
            index: 0,
            id: Some("call_return".to_owned()),
            name: Some(tool::HANDOFF_RETURN.to_owned()),
            args_delta: json!({
                "status": status,
                "summary": summary,
                "evidence": ["read the brief"],
                "open_questions": ["what next?"],
            })
            .to_string(),
        },
        ModelEvent::Finish {
            reason: StopReason::ToolCalls,
            usage: None,
        },
    ]]
}

/// A brief that passes `check_brief`.
fn brief(goal: &str, owner: &str) -> Brief {
    Brief {
        goal: goal.to_owned(),
        owner: owner.to_owned(),
        priority: Priority::Normal,
        inputs: vec![".aegis/status/STATUS.md".to_owned()],
        constraints: Vec::new(),
        definition_of_done: "the return says what you found".to_owned(),
        approval_needed: String::new(),
        return_format: ReturnFormat::Status,
    }
}

/// One `App` for a test, leaked so the runner it lends itself to is `'static`.
///
/// The process is one test binary; what is leaked is a temporary directory's
/// handles, which the operating system reclaims either way.
fn app() -> &'static App {
    Box::leak(Box::new(App::new()))
}

/// Claims 1, 2 and 5: two briefs, two sessions, one board — and one run id
/// across all of it.
#[tokio::test]
async fn two_briefs_fan_out_to_two_identities_and_come_back_as_a_board() {
    let app = app();
    let chief = app.chief();
    app.specialist("Scribe", vec![tool::FS_READ.to_owned()]);
    app.specialist("Reader", vec![tool::FS_READ.to_owned()]);
    let chief_session = app.session_as(&chief);

    let runner = TestRunner::arc(
        app,
        &chief_session,
        Script::from([
            (
                "Scribe".to_owned(),
                returns("done", "wrote the note", "thinking out loud as the Scribe"),
            ),
            (
                "Reader".to_owned(),
                returns("blocked", "the source is missing", "musing as the Reader"),
            ),
        ]),
    );

    let board = bus::deliver(
        &runner,
        HandoffPlan {
            briefs: vec![
                brief("Note what is on the board", "Scribe"),
                brief("Say what is missing", "Reader"),
            ],
            review: None,
        },
        &tokio_util::sync::CancellationToken::new(),
    )
    .await;

    // Claim 1: two of them, in the order they were written, each a status.
    assert_eq!(board.assignments.len(), 2);
    assert_eq!(board.done(), 1);
    assert_eq!(board.blocked(), 1);

    let rendered = board.render();
    assert!(rendered.contains("wrote the note"), "{rendered}");
    assert!(rendered.contains("the source is missing"), "{rendered}");

    // Claim 2: what they said to themselves stayed in their own sessions.
    let delegated: Vec<_> = app
        .sessions()
        .into_iter()
        .filter(|row| row.delegated.is_some())
        .collect();
    assert_eq!(delegated.len(), 2, "one session per brief");

    for row in &delegated {
        let said = app.said_in(&row.id);
        assert!(!said.is_empty(), "the specialist did say something");
        assert!(
            !rendered.contains(said.trim()),
            "a specialist's own words reached the board:\n{rendered}"
        );
    }
    assert!(!rendered.contains("thinking out loud"), "{rendered}");
    assert!(!rendered.contains("musing as the Reader"), "{rendered}");

    // Claim 5: one id over the whole delegation, on every line either of them
    // wrote, beside the identity that made the call.
    let lines = app.audit_lines();
    assert!(!lines.is_empty(), "the specialists' calls were audited");
    for line in &lines {
        assert_eq!(line.handoff, board.id, "{line:?}");
        assert!(!line.agent_id.is_empty(), "{line:?}");
    }

    // Each session names the delegation and the session that started it.
    for row in &delegated {
        let record = row.delegated.as_ref().expect("delegated");
        assert_eq!(record.handoff_id, board.id);
        assert_eq!(record.from_session_id, chief_session);
        let filed = record.brief.as_ref().expect("the brief was filed");
        assert!(filed.starts_with(".aegis/briefs/"), "{filed}");
        assert!(app.workspace.join(filed).is_file(), "{filed} is on disk");
    }
}

/// Claim 3, first half: a specialist runs under the identity the brief named,
/// with the tools that identity holds and no others.
#[tokio::test]
async fn a_specialist_runs_as_itself_and_is_not_offered_what_it_does_not_hold() {
    let app = app();
    let chief = app.chief();
    let scribe = app.specialist("Scribe", vec![tool::FS_READ.to_owned()]);
    let chief_session = app.session_as(&chief);

    let runner = TestRunner::arc(
        app,
        &chief_session,
        Script::from([(
            "Scribe".to_owned(),
            // It tries to write, which its identity does not allow, and then
            // returns. Both rounds are one script.
            vec![
                vec![
                    ModelEvent::ToolCallDelta {
                        index: 0,
                        id: Some("call_write".to_owned()),
                        name: Some(tool::FS_WRITE.to_owned()),
                        args_delta: json!({
                            "path": ".aegis/artefacts/note.md",
                            "content": "x",
                        })
                        .to_string(),
                    },
                    ModelEvent::Finish {
                        reason: StopReason::ToolCalls,
                        usage: None,
                    },
                ],
                returns("blocked", "I may not write", "")[0].clone(),
            ],
        )]),
    );

    let board = bus::deliver(
        &runner,
        HandoffPlan {
            briefs: vec![brief("Write the note", "Scribe")],
            review: None,
        },
        &tokio_util::sync::CancellationToken::new(),
    )
    .await;

    assert_eq!(board.blocked(), 1, "{}", board.render());

    let session = app
        .sessions()
        .into_iter()
        .find(|row| row.delegated.is_some())
        .expect("the delegated session");
    assert_eq!(session.agent_id, scribe.id, "it ran as the owner");

    // The write was refused by the identity's own allow-list, in its own
    // session — not by anything the Chief of Staff holds or does not hold.
    let refusal = app
        .audit_lines()
        .into_iter()
        .find(|line| line.tool == tool::FS_WRITE)
        .expect("the write was judged");
    assert_eq!(refusal.agent_id, scribe.id);
    assert_eq!(refusal.decision, aegis_lib::AuditDecision::Deny);
    assert_eq!(refusal.handoff, board.id);
}

/// Claim 3, second half: depth is one. A specialist is not offered
/// `handoff_delegate`, and is refused it if it asks anyway.
#[tokio::test]
async fn a_specialist_cannot_delegate_and_is_told_why() {
    let app = app();
    let chief = app.chief();
    // Granted the tool deliberately: the rule is about the *run*, not about the
    // identity, and an identity that is a Chief of Staff elsewhere is still a
    // specialist while it is working on a brief.
    app.specialist(
        "Deputy",
        vec![tool::FS_READ.to_owned(), tool::HANDOFF_DELEGATE.to_owned()],
    );
    let chief_session = app.session_as(&chief);

    let runner = TestRunner::arc(
        app,
        &chief_session,
        Script::from([(
            "Deputy".to_owned(),
            vec![
                vec![
                    ModelEvent::ToolCallDelta {
                        index: 0,
                        id: Some("call_again".to_owned()),
                        name: Some(tool::HANDOFF_DELEGATE.to_owned()),
                        args_delta: json!({
                            "briefs": [{
                                "goal": "do it for me",
                                "owner": "Deputy",
                                "definition_of_done": "it is done",
                            }],
                        })
                        .to_string(),
                    },
                    ModelEvent::Finish {
                        reason: StopReason::ToolCalls,
                        usage: None,
                    },
                ],
                returns("blocked", "I cannot re-delegate", "")[0].clone(),
            ],
        )]),
    );

    let board = bus::deliver(
        &runner,
        HandoffPlan {
            briefs: vec![brief("Pass it on", "Deputy")],
            review: None,
        },
        &tokio_util::sync::CancellationToken::new(),
    )
    .await;

    assert_eq!(board.blocked(), 1, "{}", board.render());

    let refused = app
        .audit_lines()
        .into_iter()
        .find(|line| line.tool == tool::HANDOFF_DELEGATE)
        .expect("the attempt was judged");
    assert_eq!(refused.decision, aegis_lib::AuditDecision::Deny);
    assert_eq!(
        refused.error_code.as_deref(),
        Some("E_DENIED"),
        "the allow-list is not the reason; the role is"
    );

    // Only the one session opened. Nothing was started under it.
    assert_eq!(
        app.sessions()
            .iter()
            .filter(|row| row.delegated.is_some())
            .count(),
        1
    );
}

/// Claim 4: a run that never returns is tried twice, in the same session, and
/// then goes to the human.
#[tokio::test]
async fn a_run_that_never_returns_is_tried_twice_and_then_escalates() {
    let app = app();
    let chief = app.chief();
    app.specialist("Quiet", vec![tool::FS_READ.to_owned()]);
    let chief_session = app.session_as(&chief);

    // It answers with prose and no `handoff_return`, on every attempt.
    let runner = TestRunner::arc(
        app,
        &chief_session,
        Script::from([(
            "Quiet".to_owned(),
            vec![vec![
                ModelEvent::TextDelta {
                    text: "I had a think about it.".to_owned(),
                },
                ModelEvent::Finish {
                    reason: StopReason::Stop,
                    usage: None,
                },
            ]],
        )]),
    );

    let board = bus::deliver(
        &runner,
        HandoffPlan {
            briefs: vec![brief("Say something back", "Quiet")],
            review: None,
        },
        &tokio_util::sync::CancellationToken::new(),
    )
    .await;

    assert_eq!(board.needs_you(), 1, "{}", board.render());
    assert_eq!(board.assignments[0].attempts, bus::ATTEMPTS);

    let rendered = board.render();
    assert!(
        rendered.contains("no return after 2 attempts"),
        "{rendered}"
    );
    assert!(rendered.contains("next_owner: the human"), "{rendered}");

    // Both attempts happened in one session, which is what makes a retry a
    // continuation rather than a fresh start.
    let delegated: Vec<_> = app
        .sessions()
        .into_iter()
        .filter(|row| row.delegated.is_some())
        .collect();
    assert_eq!(delegated.len(), 1, "one session, two attempts");

    let messages = app.sessions.messages(&delegated[0].id).expect("messages");
    let asked: Vec<&str> = messages
        .iter()
        .filter(|message| message.role == aegis_lib::Role::User)
        .map(|message| message.text.as_str())
        .collect();
    assert_eq!(asked.len(), 2, "it was asked twice");
    assert!(asked[1].contains("last one"), "{}", asked[1]);
}

/// Fan-in: the reviewer is given the artefacts the specialists produced, and
/// runs after them rather than beside them.
#[tokio::test]
async fn the_reviewer_runs_last_and_sees_the_artefacts_rather_than_the_work() {
    let app = app();
    let chief = app.chief();
    app.specialist("Scribe", vec![tool::FS_READ.to_owned()]);
    app.specialist("Checker", vec![tool::FS_READ.to_owned()]);
    let chief_session = app.session_as(&chief);

    std::fs::write(app.workspace.join(".aegis/artefacts/note.md"), "the note")
        .expect("an artefact");

    let runner = TestRunner::arc(
        app,
        &chief_session,
        Script::from([
            (
                "Scribe".to_owned(),
                vec![vec![
                    ModelEvent::ToolCallDelta {
                        index: 0,
                        id: Some("call_return".to_owned()),
                        name: Some(tool::HANDOFF_RETURN.to_owned()),
                        args_delta: json!({
                            "status": "done",
                            "summary": "wrote it",
                            "artefacts": [".aegis/artefacts/note.md"],
                        })
                        .to_string(),
                    },
                    ModelEvent::Finish {
                        reason: StopReason::ToolCalls,
                        usage: None,
                    },
                ]],
            ),
            (
                "Checker".to_owned(),
                returns("done", "checked it", "reading the note"),
            ),
        ]),
    );

    let board = bus::deliver(
        &runner,
        HandoffPlan {
            briefs: vec![brief("Write the note", "Scribe")],
            review: Some(brief("Check the note", "Checker")),
        },
        &tokio_util::sync::CancellationToken::new(),
    )
    .await;

    let review = board.review.as_ref().expect("a review ran");
    assert!(
        review
            .brief
            .inputs
            .contains(&".aegis/artefacts/note.md".to_owned()),
        "{:?}",
        review.brief.inputs
    );

    // Three sessions: two briefs and the review, all in the same project and
    // all under the same delegation.
    let delegated: Vec<_> = app
        .sessions()
        .into_iter()
        .filter(|row| row.delegated.is_some())
        .collect();
    assert_eq!(delegated.len(), 2, "one per brief, review included");
    for row in &delegated {
        assert_eq!(
            row.delegated.as_ref().expect("delegated").handoff_id,
            board.id
        );
    }

    let rendered = board.render();
    assert!(rendered.contains("--- review"), "{rendered}");
    assert!(!rendered.contains("reading the note"), "{rendered}");
}

/// A session a person opened is not a delegated run: it may delegate, and it is
/// not offered the tool that closes a brief.
#[tokio::test]
async fn an_ordinary_session_is_not_offered_the_tool_that_closes_a_brief() {
    let app = app();
    let chief = app.chief();
    let session_id = app.session_as(&chief);

    let provider = FakeProvider::instant();
    let cancel = app.turns.begin(&session_id, "turn-1").expect("free");
    app.sessions
        .append(&session_id, Message::user("hello"), SessionState::Running)
        .expect("stored");

    let reason = Turn {
        agent: &chief,
        sessions: &app.sessions,
        turns: &app.turns,
        grants: &app.grants,
        approvals: &app.approvals,
        audit: &app.audit,
        provider: &provider,
        sink: &app.sink,
        self_exe: None,
        captures: &app.captures,
        skills: &app.library,
        memories: &app.memories,
        connectors: aegis_lib::Connectors::none(),
        standing: Standing::Own(None),
        unattended: None,
    }
    .run(
        &TurnPlan {
            session_id: session_id.clone(),
            turn_id: "turn-1".to_owned(),
            workspace: Some(app.workspace.clone()),
        },
        &cancel,
    )
    .await;
    app.turns.finish(&session_id, "turn-1", SessionState::Idle);

    assert_eq!(reason, StopReason::Stop);
    // It holds `handoff_delegate` and was offered it; `handoff_return` closes a
    // brief and there is none here, so it is not on the list at all.
    let said = app.said_in(&session_id);
    assert!(!said.is_empty());
    assert!(
        app.sessions().iter().all(|row| row.delegated.is_none()),
        "nothing was delegated"
    );
}
