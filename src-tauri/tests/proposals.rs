//! Skill promotion, through the crate's public surface (PLAN 7.13).
//!
//! The unit tests in `skills/` cover how a proposal is listed and how an apply
//! is recognised; `policy_matrix.rs` covers the row. This file covers the exit
//! condition, which is a claim about real turns:
//!
//! > the built-in Assistant can file `skills/<name>/PROPOSAL.md` in a
//! > workspace; applying it through the gate produces a `SKILL.md` the catalog
//! > lists; `skill_run` still needs the name ticked on an identity; a proposal
//! > is never runnable; a handwritten skill is untouched; a routine still
//! > cannot name a proposal. No new tool. No editor. No grant.
//!
//! "No new tool" is not asserted by a test of its own: every step below is an
//! `fs_write` or a `skill_run`, and the registry would refuse any other name.

use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::json;
use tempfile::TempDir;

use aegis_lib::agent::event::EventSink;
use aegis_lib::agent::turn::TurnPlan;
use aegis_lib::agent::wire::{ModelEvent, StopReason};
use aegis_lib::policy::tool;
use aegis_lib::skills::{self, ProposalState};
use aegis_lib::Standing;
use aegis_lib::{
    Agent, AgentDraft, AgentStore, ApprovalDecision, ApprovalRegistry, AuditLog, Event,
    FakeProvider, Grant, GrantStore, MemoryStore, Message, SessionState, SessionStore, Turn,
    TurnRegistry, DEFAULT_PROVIDER_ID,
};

/// The name the Assistant proposes.
const NAME: &str = "brief.digest";

/// Where it proposes it, and where applying it writes.
const PROPOSAL: &str = ".aegis/skills/brief.digest/PROPOSAL.md";
const RUNBOOK: &str = ".aegis/skills/brief.digest/SKILL.md";

/// A proposal that parses, under its own heading.
fn proposed() -> String {
    skills::TRIAGE_SEED.replace("# inbox.triage", "# brief.digest")
}

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
        // The convention the way the sidebar button lays it down, which seeds
        // a handwritten `inbox.triage` into the workspace — the runbook the
        // last test makes sure this path never touches.
        aegis_lib::workspace::scaffold(&workspace).expect("the convention is laid down");

        Self {
            library: data.join(skills::LIBRARY_DIR),
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

    /// An identity somebody granted the proposed name, ahead of any runbook.
    fn digester(&self) -> Agent {
        self.agents
            .create(&AgentDraft {
                name: "Digester".to_owned(),
                role: "folds what came in into one note".to_owned(),
                instructions: String::new(),
                provider_id: DEFAULT_PROVIDER_ID.to_owned(),
                model: String::new(),
                tools: vec![
                    tool::FS_LIST.to_owned(),
                    tool::FS_READ.to_owned(),
                    tool::FS_WRITE.to_owned(),
                    tool::SKILL_RUN.to_owned(),
                    tool::SKILL_RETURN.to_owned(),
                ],
                skills: vec![NAME.to_owned()],
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

    /// Waits for the next approval the turn raises, and answers it.
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

    /// Runs one turn of scripted tool calls, answering each prompt in order.
    ///
    /// A turn that raises more prompts than `answers` holds parks and never
    /// finishes, so a row that should have asked nothing and asked anyway
    /// shows up as a hung test rather than as a pass.
    async fn turn(
        &self,
        session_id: &str,
        turn_id: &str,
        agent: &Agent,
        rounds: Vec<(&str, serde_json::Value)>,
        answers: &[ApprovalDecision],
    ) -> StopReason {
        self.sessions
            .append(session_id, Message::user("go"), SessionState::Running)
            .expect("the user's message is stored");
        let cancel = self.turns.begin(session_id, turn_id).expect("free");

        let script = rounds
            .into_iter()
            .enumerate()
            .map(|(index, (name, args))| {
                vec![
                    ModelEvent::ToolCallDelta {
                        index: 0,
                        id: Some(format!("{turn_id}_{index}")),
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
            parking: None,
            decision: None,
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

    /// The last envelope the model was handed in `session_id`.
    fn last_envelope(&self, session_id: &str) -> serde_json::Value {
        let messages = self.sessions.messages(session_id).expect("messages");
        let message = messages
            .iter()
            .rev()
            .find(|message| message.tool_call_id.is_some())
            .expect("an envelope");
        serde_json::from_str(&message.text).expect("an envelope")
    }

    fn catalog_names(&self) -> Vec<String> {
        skills::catalog(&self.library, Some(&self.workspace))
            .into_iter()
            .map(|skill| skill.name)
            .collect()
    }
}

/// The whole exit, in the order a person would live it.
#[tokio::test]
async fn a_proposal_becomes_a_runbook_only_when_a_person_applies_it_and_grants_nothing() {
    let app = App::new();
    let assistant = Agent::builtin();
    let digester = app.digester();
    let drafting = app.session_as(&assistant);

    // 1. The built-in Assistant files a proposal, through the ordinary dialog.
    app.turn(
        &drafting,
        "propose",
        &assistant,
        vec![(
            tool::FS_WRITE,
            json!({ "path": PROPOSAL, "content": proposed(), "create_dirs": true }),
        )],
        &[ApprovalDecision::AllowOnce],
    )
    .await;
    assert!(app.workspace.join(PROPOSAL).is_file());

    // Listed where a person can apply it, and nowhere a model could run it.
    let listed = skills::proposals(&app.workspace);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, NAME);
    assert_eq!(listed[0].state, ProposalState::Pending);
    assert!(!app.catalog_names().contains(&NAME.to_owned()));

    // 2. A proposal is never runnable — not even by an identity that was
    //    granted the name ahead of time — and it is refused with no dialog.
    let running = app.session_as(&digester);
    app.turn(
        &running,
        "too-early",
        &digester,
        vec![(tool::SKILL_RUN, json!({ "name": NAME }))],
        &[],
    )
    .await;
    let refused = app.last_envelope(&running);
    assert_eq!(refused["error"]["code"], "E_DENIED", "{refused}");
    let message = refused["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("only proposed"), "{message}");

    // 3. A routine cannot name it: the door is handed the catalog entry, and a
    //    proposal has none (`schedule::check`, PLAN 7.13 *Phase 16's door*).
    let catalog = skills::catalog(&app.library, Some(&app.workspace));
    assert!(skills::find(&catalog, NAME).is_none());

    // 4. Apply. Writes are allowed for the whole session, and the apply is put
    //    to a person anyway — the one `AllowOnce` below is that dialog, and the
    //    turn would never finish without it.
    app.grants.insert(&drafting, Grant::FsWrite);
    app.turn(
        &drafting,
        "apply",
        &assistant,
        vec![(
            tool::FS_WRITE,
            json!({ "path": RUNBOOK, "content": proposed() }),
        )],
        &[ApprovalDecision::AllowOnce],
    )
    .await;
    assert_eq!(app.last_envelope(&drafting)["ok"], json!(true));

    // The catalog lists it, and the proposal says it was applied.
    assert!(app.catalog_names().contains(&NAME.to_owned()));
    assert_eq!(
        skills::proposals(&app.workspace)[0].state,
        ProposalState::Applied
    );

    // 5. Applying granted nothing: the Assistant holds every tool and no
    //    skills, and still cannot run what it proposed and applied.
    app.turn(
        &drafting,
        "self-grant",
        &assistant,
        vec![(tool::SKILL_RUN, json!({ "name": NAME }))],
        &[],
    )
    .await;
    let refused = app.last_envelope(&drafting);
    assert_eq!(refused["error"]["code"], "E_DENIED", "{refused}");

    // The identity a person ticked it on can.
    app.turn(
        &running,
        "live",
        &digester,
        vec![(tool::SKILL_RUN, json!({ "name": NAME }))],
        &[],
    )
    .await;
    let loaded = app.last_envelope(&running);
    assert_eq!(loaded["ok"], json!(true), "{loaded}");
    assert_eq!(loaded["meta"]["skill"], json!(NAME));
}

/// A handwritten runbook is untouched: a proposal of the same name is filed
/// beside it, and the apply is refused before anyone is asked.
#[tokio::test]
async fn a_handwritten_runbook_is_never_replaced_by_an_apply() {
    let app = App::new();
    let assistant = Agent::builtin();
    let session = app.session_as(&assistant);
    app.grants.insert(&session, Grant::FsWrite);

    let handwritten = app.workspace.join(".aegis/skills/inbox.triage/SKILL.md");
    let before = std::fs::read_to_string(&handwritten).expect("seeded by the scaffold");
    // Different from the handwritten runbook on disk, or "untouched" would be
    // true of an overwrite with the same bytes and prove nothing.
    let rewrite = skills::TRIAGE_SEED.replace("Rewrite the file whole", "Rewrite the whole file");
    assert_ne!(rewrite, before);

    // Filing the proposal is an ordinary write, covered by the session's grant.
    // The apply is refused outright: no dialog, so no answers.
    app.turn(
        &session,
        "over",
        &assistant,
        vec![
            (
                tool::FS_WRITE,
                json!({
                    "path": ".aegis/skills/inbox.triage/PROPOSAL.md",
                    "content": rewrite,
                }),
            ),
            (
                tool::FS_WRITE,
                json!({
                    "path": ".aegis/skills/inbox.triage/SKILL.md",
                    "content": rewrite,
                }),
            ),
        ],
        &[],
    )
    .await;

    let refused = app.last_envelope(&session);
    assert_eq!(refused["error"]["code"], "E_DENIED", "{refused}");
    let message = refused["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("never replaces"), "{message}");

    assert_eq!(
        std::fs::read_to_string(&handwritten).expect("still there"),
        before
    );
    assert_eq!(
        skills::proposals(&app.workspace)[0].state,
        ProposalState::Occupied
    );
}
