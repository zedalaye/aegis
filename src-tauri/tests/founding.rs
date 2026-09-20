//! Cabinet founding, through the crate's public surface (PLAN 7.14).
//!
//! The unit tests in `roster.rs` cover how a roster parses, previews and
//! applies; `store/agents.rs` covers the batch. This file covers the exit
//! condition, which is a claim about real turns:
//!
//! > a Duplicate of the built-in Assistant, once granted `cabinet.found`, can
//! > file `.aegis/roster/PROPOSAL.md` in a workspace; applying it in Settings
//! > creates the named identities with the named allow-lists and does not
//! > create routines, connectors, or a world; a name that already exists is
//! > skipped; the built-in Assistant is unchanged and still holds no skills; a
//! > routine still cannot be saved without a witnessed run; a session cannot
//! > apply. No new tool. No wizard. No Edit on the built-in row. The grant is
//! > the apply.
//!
//! "Applying it in Settings" is [`roster::apply`], which is all the command
//! does besides announcing the audit lines.

use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::json;
use tempfile::TempDir;

use aegis_lib::agent::event::EventSink;
use aegis_lib::agent::turn::TurnPlan;
use aegis_lib::agent::wire::{ModelEvent, StopReason};
use aegis_lib::policy::tool;
use aegis_lib::roster::{self, RosterEntryState, ROSTER_FILE};
use aegis_lib::skills;
use aegis_lib::Standing;
use aegis_lib::{
    Agent, AgentDraft, AgentStore, ApprovalDecision, ApprovalRegistry, AuditLog, Event,
    FakeProvider, GrantStore, MemoryStore, Message, RoutineDraft, Schedule, SessionState,
    SessionStore, Turn, TurnRegistry, DEFAULT_AGENT_ID, DEFAULT_PROVIDER_ID,
};

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

/// A data directory with a seeded library, a scaffolded workspace, and every
/// store the runtime holds.
struct App {
    _dir: TempDir,
    data: PathBuf,
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
        aegis_lib::workspace::scaffold(&workspace).expect("the convention is laid down");
        let library = data.join(skills::LIBRARY_DIR);
        skills::seed(&library);

        Self {
            library,
            captures: data.join("captures"),
            agents: AgentStore::load(&data),
            sessions: SessionStore::load(&data),
            memories: MemoryStore::load(&data),
            audit: AuditLog::new(&data),
            data,
            workspace,
            _dir: dir,
            turns: TurnRegistry::new(),
            grants: GrantStore::new(),
            approvals: ApprovalRegistry::new(),
        }
    }

    /// What Duplicate on the built-in row opens, with a role typed in and the
    /// one tick. A row a person made, not `default`.
    fn founder(&self) -> Agent {
        let assistant = Agent::builtin();
        self.agents
            .create(&AgentDraft {
                name: "Assistant copy".to_owned(),
                role: "drafts the cabinet for this project".to_owned(),
                instructions: assistant.instructions,
                provider_id: DEFAULT_PROVIDER_ID.to_owned(),
                model: String::new(),
                tools: assistant.tools,
                skills: vec![skills::FOUND_SKILL.to_owned()],
                runs_per_day: assistant.runs_per_day,
            })
            .expect("the duplicate is accepted")
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
    /// finishes, so a call that should have asked nothing shows up as a hung
    /// test rather than as a pass.
    async fn turn(
        &self,
        session_id: &str,
        turn_id: &str,
        agent: &Agent,
        rounds: Vec<(&str, serde_json::Value)>,
        answers: &[ApprovalDecision],
    ) {
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
        tokio::join!(running, answering);

        self.turns.finish(session_id, turn_id, SessionState::Idle);
    }

    /// Every envelope the model was handed in `session_id`, in order.
    fn envelopes(&self, session_id: &str) -> Vec<serde_json::Value> {
        self.sessions
            .messages(session_id)
            .expect("messages")
            .iter()
            .filter(|message| message.tool_call_id.is_some())
            .map(|message| serde_json::from_str(&message.text).expect("an envelope"))
            .collect()
    }

    fn catalog(&self) -> Vec<skills::Skill> {
        skills::catalog(&self.library, Some(&self.workspace))
    }
}

/// The roster `cabinet.found` tells a founder to write, de-indented.
fn default_roster() -> String {
    skills::FOUND_SEED
        .lines()
        .skip_while(|line| *line != "    # Roster")
        .take_while(|line| line.is_empty() || line.starts_with("    "))
        .map(|line| line.strip_prefix("    ").unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The whole exit, in the order a person would live it.
#[tokio::test]
async fn a_duplicate_proposes_the_cabinet_and_only_the_apply_creates_it() {
    let app = App::new();
    let assistant = Agent::builtin();

    // 0. The built-in row is not the founder. It holds no skills, and a tick on
    //    it is `agent_update` of a constant, which is refused.
    let edit = app.agents.update(
        DEFAULT_AGENT_ID,
        &AgentDraft {
            name: assistant.name.clone(),
            role: "founder".to_owned(),
            instructions: String::new(),
            provider_id: DEFAULT_PROVIDER_ID.to_owned(),
            model: String::new(),
            tools: assistant.tools.clone(),
            skills: vec![skills::FOUND_SKILL.to_owned()],
            runs_per_day: assistant.runs_per_day,
        },
    );
    assert!(edit.is_err(), "no Edit on the built-in row");

    let as_builtin = app.session_as(&assistant);
    app.turn(
        &as_builtin,
        "builtin",
        &assistant,
        vec![(tool::SKILL_RUN, json!({ "name": skills::FOUND_SKILL }))],
        &[],
    )
    .await;
    let refused = app.envelopes(&as_builtin).pop().expect("an envelope");
    assert_eq!(refused["error"]["code"], "E_DENIED", "{refused}");

    // 1. A Duplicate with the one tick loads the runbook and files the roster,
    //    through the ordinary dialog.
    let founder = app.founder();
    let founding = app.session_as(&founder);
    app.turn(
        &founding,
        "found",
        &founder,
        vec![
            (tool::SKILL_RUN, json!({ "name": skills::FOUND_SKILL })),
            (
                tool::FS_WRITE,
                json!({ "path": ROSTER_FILE, "content": default_roster(), "create_dirs": true }),
            ),
        ],
        &[ApprovalDecision::AllowOnce],
    )
    .await;
    let envelopes = app.envelopes(&founding);
    assert_eq!(envelopes[0]["ok"], json!(true), "{}", envelopes[0]);
    assert_eq!(envelopes[0]["meta"]["skill"], json!(skills::FOUND_SKILL));
    assert_eq!(envelopes[1]["ok"], json!(true), "{}", envelopes[1]);
    assert!(app.workspace.join(ROSTER_FILE).is_file());

    // Writing it created nobody.
    assert_eq!(app.agents.list().len(), 2, "the Assistant and its copy");

    // 2. A session cannot apply. There is no tool to call, and the names a
    //    model might reach for are refused like any name nothing answers to.
    for name in [roster::AGENT_CREATE, "agent_update", "roster_apply"] {
        assert!(!aegis_lib::tools::names().contains(&name), "{name}");
    }
    app.turn(
        &founding,
        "self-apply",
        &founder,
        vec![
            ("roster_apply", json!({ "path": ROSTER_FILE })),
            (roster::AGENT_CREATE, json!({ "name": "Chief of Staff" })),
        ],
        &[],
    )
    .await;
    for envelope in app.envelopes(&founding).iter().rev().take(2) {
        assert_ne!(envelope["ok"], json!(true), "{envelope}");
    }
    assert_eq!(app.agents.list().len(), 2, "still nobody new");

    // 3. The preview is the allow-lists, identity by identity.
    let catalog = app.catalog();
    let shown = roster::read(&app.workspace, &app.agents, &[], &catalog).expect("a proposal");
    assert!(shown.appliable, "{shown:?}");
    assert_eq!(shown.entries.len(), 2);
    for entry in &shown.entries {
        assert_eq!(entry.state, RosterEntryState::New);
        assert!(entry.notes.is_empty(), "{}: {:?}", entry.name, entry.notes);
    }

    // 4. Apply is the grant.
    let (applied, lines) = roster::apply(
        &app.workspace,
        &app.agents,
        &[],
        &catalog,
        &shown.digest,
        &app.audit,
        "project-1",
    )
    .expect("applied");
    assert_eq!(applied.created.len(), 2);
    assert_eq!(lines.len(), 2);

    let chief = applied
        .created
        .iter()
        .find(|agent| agent.name == "Chief of Staff")
        .expect("the Chief");
    assert_eq!(chief.tools, shown.entries[0].tools);
    assert_eq!(chief.skills, shown.entries[0].skills);
    assert!(!chief.allows(tool::SHELL_EXEC));
    assert_eq!(chief.runs_per_day, 0);

    let reviewer = applied
        .created
        .iter()
        .find(|agent| agent.name == "Reviewer")
        .expect("the Reviewer");
    assert!(!reviewer.allows(tool::FS_WRITE));
    assert!(!reviewer.allows(tool::SHELL_EXEC));

    // Nothing but identities.
    assert!(!app.data.join("routines.json").exists());
    assert!(!app.data.join("connectors.json").exists());
    assert!(!app.workspace.join("world").exists());

    // The built-in Assistant is unchanged and still holds no skills.
    let builtin = app.agents.list().into_iter().next().expect("listed first");
    assert_eq!(builtin, Agent::builtin());
    assert!(builtin.skills.is_empty());

    // 5. A routine still cannot be saved without a witnessed run. The Chief was
    //    granted `cos.loop` by the apply, and nobody has watched it run.
    let loop_skill = skills::find(&catalog, skills::COS_SKILL);
    let door = aegis_lib::schedule::check(
        &RoutineDraft {
            name: "morning loop".to_owned(),
            project_id: "project-1".to_owned(),
            agent_id: chief.id.clone(),
            skill: skills::COS_SKILL.to_owned(),
            schedule: Schedule::Every { minutes: 60 },
            grants: Vec::new(),
            runs_per_day: 1,
        },
        chief,
        loop_skill,
        app.audit.witnessed(&chief.id, skills::COS_SKILL),
    )
    .expect_err("refused");
    assert!(door.to_string().contains("under watch"), "{door}");

    // 6. Founding again skips every name that now exists.
    let again = roster::read(&app.workspace, &app.agents, &[], &catalog).expect("a proposal");
    assert!(again
        .entries
        .iter()
        .all(|entry| entry.state == RosterEntryState::Present));
    assert!(!again.appliable, "nothing left to create");
}
