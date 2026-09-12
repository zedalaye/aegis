//! The skill runner, through the crate's public surface.
//!
//! The unit tests inside `skills/` cover the format, the catalog, the handoff
//! rules and the two tools. This file covers the Phase 13 exit condition of
//! `PLAN.md` § 7.3, which is a claim about the *whole* runtime:
//!
//! > the catalog is listable; one global skill (e.g. never-send-without-review)
//! > and one workspace stub (`inbox.triage`: file in, status + artefact out)
//! > run end-to-end; the body is absent from the system prompt of turns that
//! > did not invoke it.
//!
//! Four claims, in the order they matter:
//!
//! 1. **The catalog is listable, and it is a catalog.** Both scopes are found,
//!    the identity's allow-list narrows them, and what reaches the system
//!    message is a line per runbook — never a step.
//! 2. **A run goes end-to-end.** `skill_run` hands the body over, the steps
//!    are carried out with the ordinary tools through the ordinary gate, and
//!    `skill_return` closes it with a status object that had to pass. The
//!    workspace stub does what its name says: file in, status and artefact
//!    out.
//! 3. **The body does not linger.** A turn that did not invoke the skill has
//!    no step of it in its system message.
//! 4. **A run is on the record.** Every audit line between the two skill calls
//!    carries the skill's name, which is what makes a run budgetable and
//!    replayable (PLAN 7.6).
//!
//! And one claim in the other direction, which is what keeps this phase a
//! no-op for everything before it: an identity granted no skills sends the
//! request Phase 12 sent.
//!
//! The command layer above this needs a running Tauri application and is not
//! reachable from a test binary. Everything below it is, against real files in
//! a temporary directory.

use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::json;
use tempfile::TempDir;

use aegis_lib::agent::event::EventSink;
use aegis_lib::agent::transcript;
use aegis_lib::agent::turn::TurnPlan;
use aegis_lib::agent::wire::{ModelEvent, StopReason, WireMessage};
use aegis_lib::policy::tool;
use aegis_lib::skills::{self, SkillScope};
use aegis_lib::Standing;
use aegis_lib::{
    Agent, AgentDraft, AgentStore, ApprovalRegistry, AuditEntry, AuditLog, Event, FakeProvider,
    GrantStore, MemoryStore, Message, SessionState, SessionStore, Turn, TurnRegistry,
    DEFAULT_PROVIDER_ID,
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

/// A data directory with a seeded library, a scaffolded workspace, and every
/// store the runtime holds.
struct App {
    _dir: TempDir,
    workspace: PathBuf,
    library: PathBuf,
    agents: AgentStore,
    sessions: SessionStore,
    turns: TurnRegistry,
    grants: GrantStore,
    approvals: ApprovalRegistry,
    audit: AuditLog,
    captures: PathBuf,
    /// An empty memory store: these files are about runbooks.
    memories: MemoryStore,
}

impl App {
    fn new() -> Self {
        let dir = TempDir::new().expect("temp dir");
        let data = dir.path().join("data");
        let workspace = dir.path().join("work");
        std::fs::create_dir_all(&data).expect("data dir");
        std::fs::create_dir_all(&workspace).expect("workspace dir");

        let workspace = dunce::canonicalize(&workspace).expect("canonical workspace");

        // The library the way the application lays it down, and the workspace
        // convention the way the sidebar button lays it down — so what these
        // tests run is what a user gets, not a fixture that happens to agree
        // with it.
        let library = data.join(skills::LIBRARY_DIR);
        skills::seed(&library);
        aegis_lib::workspace::scaffold(&workspace).expect("the convention is laid down");

        // The item the triage runbook is pointed at. A markdown file in
        // `.aegis/briefs/` is a valid input today; a mail connector later replaces the
        // source, not the procedure (PLAN 7.6).
        std::fs::write(
            workspace.join(".aegis/briefs/from-a-client.md"),
            "The staging deploy is failing since Tuesday. Can you look before Friday?\n",
        )
        .expect("an item to triage");

        Self {
            workspace,
            library,
            _dir: dir,
            agents: AgentStore::load(&data),
            sessions: SessionStore::load(&data),
            turns: TurnRegistry::new(),
            grants: GrantStore::new(),
            approvals: ApprovalRegistry::new(),
            audit: AuditLog::new(&data),
            captures: data.join("captures"),
            memories: MemoryStore::load(&data),
        }
    }

    /// An identity granted both seeded runbooks and the tools they call.
    fn triager(&self) -> Agent {
        self.agents
            .create(&AgentDraft {
                name: "Triager".to_owned(),
                role: "sorts what comes in into the board".to_owned(),
                instructions: String::new(),
                provider_id: DEFAULT_PROVIDER_ID.to_owned(),
                tools: vec![
                    tool::FS_LIST.to_owned(),
                    tool::FS_READ.to_owned(),
                    tool::FS_WRITE.to_owned(),
                    tool::SKILL_RUN.to_owned(),
                    tool::SKILL_RETURN.to_owned(),
                ],
                skills: vec!["inbox.triage".to_owned(), skills::REVIEW_SKILL.to_owned()],
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

    /// The system message the *next* turn of `session_id` would send,
    /// assembled the way the turn loop assembles it.
    fn next_system_message(&self, session_id: &str, agent: &Agent) -> String {
        let history = self.sessions.messages(session_id).expect("messages");
        let catalog = skills::catalog(&self.library, Some(&self.workspace));
        let block = skills::prompt_block(&skills::granted(&catalog, agent));
        let shared = aegis_lib::workspace::digest(&self.workspace);

        let request = transcript::build(
            "m",
            &transcript::Context {
                agent,
                workspace: Some(&self.workspace),
                exec_host: None,
                memories: None,
                skills: block.as_deref(),
                world: None,
                shared: shared.as_deref(),
                compacted: None,
                unattended: false,
            },
            &history,
            aegis_lib::tools::schemas_for(&agent.tools, &aegis_lib::ConnectorCatalog::empty()),
        );

        match request.messages.first() {
            Some(WireMessage::System { content }) => content.clone(),
            other => panic!("the first message is not a system message: {other:?}"),
        }
    }

    /// Runs one turn whose rounds are scripted, one tool call per round.
    ///
    /// Nothing answers an approval, deliberately: the two `fs_write` calls the
    /// triage runbook makes are inside `.aegis/briefs/`-adjacent folders and *do* ask,
    /// so the caller supplies a session grant first and the turn never parks.
    async fn scripted_turn(
        &self,
        session_id: &str,
        agent: &Agent,
        sink: &Recorder,
        rounds: Vec<(&str, serde_json::Value)>,
    ) -> StopReason {
        self.scripted_turn_at(session_id, agent, sink, "turn-1", rounds)
            .await
    }

    /// The same, for a test that needs more than one turn of a session.
    async fn scripted_turn_at(
        &self,
        session_id: &str,
        agent: &Agent,
        sink: &Recorder,
        turn_id: &str,
        rounds: Vec<(&str, serde_json::Value)>,
    ) -> StopReason {
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

    /// Every audit line written so far, oldest first.
    fn audit_lines(&self) -> Vec<AuditEntry> {
        let mut lines = self.audit.tail(100, None).expect("tail");
        lines.reverse();
        lines
    }

    /// The envelopes the model was handed, oldest first.
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

/// Claim 1: both scopes are discovered, the allow-list narrows them, and what
/// reaches the model is a catalog rather than a library.
#[test]
fn the_catalog_is_listable_and_carries_no_step_of_any_runbook() {
    let app = App::new();
    let triager = app.triager();
    let session_id = app.session_as(&triager);
    app.say(&session_id, "anything come in?");

    let catalog = skills::catalog(&app.library, Some(&app.workspace));
    let names: Vec<&str> = catalog.iter().map(|skill| skill.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            skills::ALERT_SKILL,
            skills::COS_SKILL,
            skills::DEPLOY_SKILL,
            "inbox.triage",
            skills::MAIL_SKILL,
            skills::REVIEW_SKILL,
            skills::REPLY_SKILL,
            skills::REVIEW_DIFF_SKILL,
            skills::THREAD_SKILL,
            skills::CHECK_SKILL,
            skills::DRAFT_SKILL,
            skills::PERCEIVE_SKILL,
            skills::VERIFY_SKILL,
        ]
    );

    // Two scopes, and each found where `COS.md` says it lives: the standing
    // rules in the user's library, the project's own procedure in the project.
    let scopes: Vec<SkillScope> = catalog.iter().map(|skill| skill.scope).collect();
    assert_eq!(
        scopes,
        vec![
            SkillScope::Library,
            SkillScope::Library,
            SkillScope::Library,
            SkillScope::Workspace,
            SkillScope::Library,
            SkillScope::Library,
            SkillScope::Library,
            SkillScope::Library,
            SkillScope::Library,
            SkillScope::Library,
            SkillScope::Library,
            SkillScope::Library,
            SkillScope::Library,
        ]
    );
    assert!(catalog.iter().all(aegis_lib::Skill::runnable));

    let system = app.next_system_message(&session_id, &triager);
    assert!(system.contains("inbox.triage"), "{system}");
    assert!(system.contains(skills::REVIEW_SKILL), "{system}");
    assert!(
        !system.contains("Rewrite the file whole"),
        "a step of the triage runbook reached the system message:\n{system}"
    );
    assert!(
        !system.contains("Sending is not a step"),
        "a step of the review runbook reached the system message:\n{system}"
    );

    // The third scope is the identity, and it is a filter rather than a place.
    let narrow = Agent {
        skills: vec![skills::REVIEW_SKILL.to_owned()],
        ..triager.clone()
    };
    let offered = skills::granted(&catalog, &narrow);
    assert_eq!(offered.len(), 1);
    assert_eq!(offered[0].name, skills::REVIEW_SKILL);
}

/// Claim 2, and the exit criterion itself: the workspace stub runs end to end.
/// A file goes in, a status and an artefact come out, and the steps in between
/// are ordinary tool calls through the ordinary gate.
#[tokio::test]
async fn the_workspace_stub_runs_end_to_end_and_its_return_is_validated() {
    let app = App::new();
    let triager = app.triager();
    let session_id = app.session_as(&triager);
    app.say(&session_id, "triage what came in");

    // A write inside the workspace is an ask; the user allowing it for the
    // session is what a person would do here, and it is the only thing this
    // test stands in for.
    app.grants.insert(&session_id, aegis_lib::Grant::FsWrite);

    let status =
        std::fs::read_to_string(app.workspace.join(".aegis/status/STATUS.md")).expect("seeded");
    let sink = Recorder::default();

    let reason = app
        .scripted_turn(
            &session_id,
            &triager,
            &sink,
            vec![
                // Load the runbook.
                (tool::SKILL_RUN, json!({ "name": "inbox.triage" })),
                // Its steps, as an identity that holds these tools.
                (
                    tool::FS_READ,
                    json!({ "path": ".aegis/briefs/from-a-client.md" }),
                ),
                (
                    tool::FS_WRITE,
                    json!({
                        "path": ".aegis/artefacts/from-a-client.triage.md",
                        "content": "# Triage\n\nAsked: look at the failing staging deploy.\nFor: \
                                    the client.\nBlocked on: nothing.\nUrgent: before Friday.\n",
                    }),
                ),
                (
                    tool::FS_WRITE,
                    json!({
                        "path": ".aegis/status/STATUS.md",
                        "content": format!("{status}\n- staging deploy, before Friday\n"),
                    }),
                ),
                // Close the run.
                (
                    tool::SKILL_RETURN,
                    json!({
                        "status": "done",
                        "summary": "A client reports staging failing since Tuesday.\nFiled under \
                                    In flight; wanted before Friday.",
                        "artefacts": [
                            ".aegis/artefacts/from-a-client.triage.md",
                            ".aegis/status/STATUS.md",
                        ],
                        "evidence": ["read .aegis/briefs/from-a-client.md"],
                        "next_owner": "human",
                    }),
                ),
            ],
        )
        .await;

    assert_eq!(reason, StopReason::Stop, "the turn finishes cleanly");
    assert!(!sink.names().contains(&"turn:error"), "{:?}", sink.names());

    // File in, status and artefact out — the stub doing what its name says.
    assert!(app
        .workspace
        .join(".aegis/artefacts/from-a-client.triage.md")
        .is_file());
    assert!(
        std::fs::read_to_string(app.workspace.join(".aegis/status/STATUS.md"))
            .expect("read")
            .contains("staging deploy"),
        "the board was updated"
    );

    let envelopes = app.envelopes(&session_id);
    assert_eq!(envelopes.len(), 5);

    // The body arrived, and only here.
    let loaded = envelopes[0]["content"].as_str().unwrap_or_default();
    assert!(loaded.contains("Rewrite the file whole"), "{loaded}");
    assert_eq!(envelopes[0]["meta"]["skill"], json!("inbox.triage"));

    // And the return was checked rather than believed: `COS.md`'s shape, with
    // the artefacts it names verified against the disk.
    let returned = &envelopes[4];
    assert_eq!(returned["ok"], json!(true), "{returned}");
    let rendered = returned["content"].as_str().unwrap_or_default();
    assert!(rendered.contains("status: done"), "{rendered}");
    assert!(
        rendered.contains(".aegis/artefacts/from-a-client.triage.md"),
        "{rendered}"
    );
    assert!(rendered.contains("next_owner: human"), "{rendered}");
}

/// The check with the most value in the phase: a `done` naming a file nobody
/// wrote is refused, and the run stays open so the model can correct itself.
#[tokio::test]
async fn a_run_that_claims_an_artefact_it_never_wrote_is_refused() {
    let app = App::new();
    let triager = app.triager();
    let session_id = app.session_as(&triager);
    app.say(&session_id, "triage what came in");

    let sink = Recorder::default();
    app.scripted_turn(
        &session_id,
        &triager,
        &sink,
        vec![
            (tool::SKILL_RUN, json!({ "name": "inbox.triage" })),
            (
                tool::SKILL_RETURN,
                json!({
                    "status": "done",
                    "summary": "Triaged it and filed the note.",
                    "artefacts": [".aegis/artefacts/from-a-client.triage.md"],
                }),
            ),
        ],
    )
    .await;

    let envelopes = app.envelopes(&session_id);
    let refused = &envelopes[1];
    assert_eq!(refused["ok"], json!(false), "{refused}");
    let message = refused["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("from-a-client.triage.md"), "{message}");
    assert!(
        message.contains("blocked"),
        "it says what to do instead: {message}"
    );

    // The refusal is still audited as part of the run: a corrected second
    // attempt has to be readable as the same skill's.
    let lines = app.audit_lines();
    assert_eq!(lines.len(), 2);
    assert!(
        lines.iter().all(|line| line.skill == "inbox.triage"),
        "{lines:?}"
    );
}

/// Claim 4: every call between the two skill calls carries the run's name, so
/// "what did this runbook actually do" is answerable afterwards (PLAN 7.6).
/// And a call made outside a run carries none, so the two are distinguishable.
#[tokio::test]
async fn every_audit_line_of_a_run_names_the_skill_and_lines_outside_it_do_not() {
    let app = App::new();
    let triager = app.triager();
    let session_id = app.session_as(&triager);
    app.say(&session_id, "have a look, then triage it");

    let sink = Recorder::default();
    app.scripted_turn(
        &session_id,
        &triager,
        &sink,
        vec![
            // Before the run.
            (tool::FS_LIST, json!({ "path": "briefs" })),
            (tool::SKILL_RUN, json!({ "name": "inbox.triage" })),
            // Inside it.
            (
                tool::FS_READ,
                json!({ "path": ".aegis/briefs/from-a-client.md" }),
            ),
            (
                tool::SKILL_RETURN,
                json!({
                    "status": "blocked",
                    "summary": "Read the item; could not write the board.",
                    "open_questions": ["may I update STATUS.md?"],
                }),
            ),
            // After it.
            (tool::FS_LIST, json!({ "path": "artefacts" })),
        ],
    )
    .await;

    let skills_named: Vec<(String, String)> = app
        .audit_lines()
        .into_iter()
        .map(|line| (line.tool, line.skill))
        .collect();

    assert_eq!(
        skills_named,
        vec![
            (tool::FS_LIST.to_owned(), String::new()),
            (tool::SKILL_RUN.to_owned(), "inbox.triage".to_owned()),
            (tool::FS_READ.to_owned(), "inbox.triage".to_owned()),
            (tool::SKILL_RETURN.to_owned(), "inbox.triage".to_owned()),
            (tool::FS_LIST.to_owned(), String::new()),
        ]
    );
}

/// Claim 3: a turn that did not invoke a skill carries no step of one — the
/// property "catalog in, body on demand" exists for, asserted on the message
/// that actually gets sent.
#[tokio::test]
async fn the_body_is_gone_from_the_turn_after_the_one_that_loaded_it() {
    let app = App::new();
    let triager = app.triager();
    let session_id = app.session_as(&triager);
    app.say(&session_id, "read the runbook");

    let sink = Recorder::default();
    app.scripted_turn(
        &session_id,
        &triager,
        &sink,
        vec![(tool::SKILL_RUN, json!({ "name": skills::REVIEW_SKILL }))],
    )
    .await;

    // It arrived, once, as the result of the call that asked for it.
    let loaded = app.envelopes(&session_id)[0]["content"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(loaded.contains("Sending is not a step"), "{loaded}");

    // And the next turn's system message does not carry it — only the line
    // that says the runbook exists.
    app.say(&session_id, "and now something else");
    let system = app.next_system_message(&session_id, &triager);

    assert!(
        system.contains(skills::REVIEW_SKILL),
        "the catalog line stays"
    );
    assert!(
        !system.contains("Sending is not a step"),
        "the runbook is in the system message of a turn that did not ask for it:\n{system}"
    );
}

/// The other direction: an identity granted no skills sends what Phase 12
/// sent. A phase that changed every existing session would be a migration
/// nobody asked for.
#[test]
fn an_identity_granted_no_skills_is_left_exactly_as_it_was() {
    let app = App::new();
    let builtin = Agent::builtin();
    let session_id = app.session_as(&builtin);
    app.say(&session_id, "hello");

    let system = app.next_system_message(&session_id, &builtin);

    assert!(!system.contains("Skills you may run"), "{system}");
    assert!(!system.contains("inbox.triage"), "{system}");
    assert!(
        system.contains("The workspace is:"),
        "and everything else is still there: {system}"
    );
}

/// A skill nobody granted is refused before the runbook is even located, and
/// with no dialog: an allow-list is not something a user is prompted past.
#[tokio::test]
async fn a_skill_outside_the_allow_list_never_reaches_the_library() {
    let app = App::new();
    let narrow = app
        .agents
        .create(&AgentDraft {
            name: "Reviewer".to_owned(),
            role: "checks drafts before they leave".to_owned(),
            instructions: String::new(),
            provider_id: DEFAULT_PROVIDER_ID.to_owned(),
            tools: vec![
                tool::FS_READ.to_owned(),
                tool::SKILL_RUN.to_owned(),
                tool::SKILL_RETURN.to_owned(),
            ],
            skills: vec![skills::REVIEW_SKILL.to_owned()],
            runs_per_day: 24,
        })
        .expect("the identity is accepted");

    let session_id = app.session_as(&narrow);
    app.say(&session_id, "triage the inbox");

    let sink = Recorder::default();
    app.scripted_turn(
        &session_id,
        &narrow,
        &sink,
        vec![(tool::SKILL_RUN, json!({ "name": "inbox.triage" }))],
    )
    .await;

    assert!(
        !sink.names().contains(&"tool:approval_required"),
        "{:?}",
        sink.names()
    );

    let refused = &app.envelopes(&session_id)[0];
    assert_eq!(refused["error"]["code"], "E_DENIED");
    let message = refused["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("Reviewer"), "{message}");
    assert!(message.contains("inbox.triage"), "{message}");
}

/// PLAN 7.6, *No extra rights*: a runbook the identity cannot carry out is
/// refused before its first step, rather than partway through it.
#[tokio::test]
async fn a_runbook_the_identity_cannot_carry_out_fails_closed() {
    let app = App::new();
    let reader = app
        .agents
        .create(&AgentDraft {
            name: "Reader".to_owned(),
            role: "reads and reports, and changes nothing".to_owned(),
            instructions: String::new(),
            provider_id: DEFAULT_PROVIDER_ID.to_owned(),
            // Granted the skill, and not the `fs_write` its steps call.
            tools: vec![
                tool::FS_LIST.to_owned(),
                tool::FS_READ.to_owned(),
                tool::SKILL_RUN.to_owned(),
                tool::SKILL_RETURN.to_owned(),
            ],
            skills: vec!["inbox.triage".to_owned()],
            runs_per_day: 24,
        })
        .expect("the identity is accepted");

    let session_id = app.session_as(&reader);
    app.say(&session_id, "triage what came in");

    let sink = Recorder::default();
    app.scripted_turn(
        &session_id,
        &reader,
        &sink,
        vec![(tool::SKILL_RUN, json!({ "name": "inbox.triage" }))],
    )
    .await;

    let refused = &app.envelopes(&session_id)[0];
    assert_eq!(refused["ok"], json!(false), "{refused}");
    assert_eq!(refused["error"]["code"], "E_DENIED");
    let message = refused["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains(tool::FS_WRITE), "{message}");
    assert!(
        message.contains("grants nothing"),
        "a skill is not a back door around policy: {message}"
    );

    // Nothing of the runbook reached the model, so nothing of it can be
    // followed by hand around the refusal.
    let content = refused["content"].as_str().unwrap_or_default();
    assert!(content.is_empty(), "{content}");
}

/// Claim 5, written from a run of the Phase 19 `review.diff` runbook against a
/// real diff: **a run survives the turn boundary the round cap creates.**
///
/// `IDEAS.md` § 10 has the trace. One review took five turns, because the cap
/// ends a turn at eight rounds; the run's name lived in a turn local; so every
/// audit line after the first boundary carried no skill — the `fs_write` of the
/// artefact the run existed to produce among them — and the closing
/// `skill_return` was refused, because by then nothing was open to close. PLAN
/// 7.6 asks one thing of a run, that it can be budgeted and replayed, and a
/// name that stops at the first boundary cannot deliver it.
#[tokio::test]
async fn a_run_carries_across_turns_and_closes_in_a_later_one() {
    let app = App::new();
    let triager = app.triager();
    let session_id = app.session_as(&triager);
    app.grants.insert(&session_id, aegis_lib::Grant::FsWrite);
    let sink = Recorder::default();

    // Turn one opens the run and ends without returning, which is what the
    // round cap does to a procedure half-way through.
    app.say(&session_id, "triage what came in");
    app.scripted_turn_at(
        &session_id,
        &triager,
        &sink,
        "turn-a",
        vec![(tool::SKILL_RUN, json!({ "name": "inbox.triage" }))],
    )
    .await;
    assert_eq!(
        app.turns.open_run(&session_id).as_deref(),
        Some("inbox.triage"),
        "the run is still open between the two turns"
    );

    // Turn two does the work and closes it, having opened nothing itself.
    app.say(&session_id, "carry on");
    app.scripted_turn_at(
        &session_id,
        &triager,
        &sink,
        "turn-b",
        vec![
            (
                tool::FS_WRITE,
                json!({
                    "path": ".aegis/artefacts/from-a-client.triage.md",
                    "content": "Asked: why staging fails. For: the client. Urgent: Friday.\n",
                }),
            ),
            (
                tool::SKILL_RETURN,
                json!({
                    "status": "done",
                    "summary": "Triaged it and filed the note.",
                    "artefacts": [".aegis/artefacts/from-a-client.triage.md"],
                    "evidence": ["read .aegis/briefs/from-a-client.md"],
                }),
            ),
        ],
    )
    .await;

    // The return is accepted in a turn that never called `skill_run`. This is
    // the call that failed with `E_TOOL_FAILED` in the trace.
    let envelopes = app.envelopes(&session_id);
    let returned = envelopes.last().expect("the return's envelope");
    assert_eq!(returned["ok"], json!(true), "{returned}");
    assert_eq!(returned["meta"]["skill"], json!("inbox.triage"));
    assert_eq!(returned["meta"]["status"], json!("done"));

    // And every line of the second turn is on the run's record, the artefact
    // write included — which is what "budgeted and replayed" needs.
    let lines = app.audit_lines();
    let second: Vec<&AuditEntry> = lines
        .iter()
        .filter(|line| line.turn_id == "turn-b")
        .collect();
    assert!(!second.is_empty(), "the second turn wrote audit lines");
    for line in second {
        assert_eq!(
            line.skill, "inbox.triage",
            "`{}` was recorded outside the run",
            line.tool
        );
    }

    // Closed by the return, not left open over whatever is said next.
    assert_eq!(app.turns.open_run(&session_id), None);
}
