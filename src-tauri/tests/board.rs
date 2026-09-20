//! The status board and the replay, through the crate's public surface.
//!
//! Phase 17's exit condition (PLAN 7.3): who ran, what it cost and why it
//! failed, from real routine runs rather than hand-built audit lines.
//!
//! 1. A successful run reports identity, cost and artefacts from the record.
//! 2. A failed run says why and lands in the right column.
//! 3. The board merges file and runtime lines, each labelled.

use std::path::PathBuf;
use std::sync::Mutex;

use tempfile::TempDir;

use aegis_lib::agent::event::EventSink;
use aegis_lib::board::{self, trace};
use aegis_lib::policy::tool;
use aegis_lib::schedule::runner;
use aegis_lib::skills;
use aegis_lib::store::routines::{RoutineDraft, Schedule};
use aegis_lib::{
    Agent, AgentDraft, AgentStore, ApprovalRegistry, AuditEntry, AuditLog, Event, FakeProvider,
    Grant, GrantStore, MemoryStore, Provider, RoutineStore, SessionStore, Store, TurnRegistry,
    DEFAULT_PROVIDER_ID,
};

/// The runbook the routine fires: file in, status out.
const WATCH_SKILL: &str = "watch.digest";

const WATCH_RUNBOOK: &str = r#"---
version: 1
tools: fs_read, fs_write
---

# watch.digest

## When to use it
On a schedule, to record what changed since the last look.

## Inputs required and tools it will call
`.aegis/briefs/` in this workspace. Calls `fs_read` and `fs_write`.

## Steps
1. Read what is in the workspace.
2. Write a line into `.aegis/status/`.

## How to validate
The status file exists and names today.

## What to return
`done` with the status file as an artefact.

## What requires approval
The write. A routine signs for it once, when it is saved.

## What to do if the source is missing
Return `blocked` and name the folder.
"#;

/// A board somebody has been keeping by hand.
const STATUS: &str = "\
# Status

What is true right now.

## Attention

- Chase the signed quote

## In flight

_Nothing running._

## Blocked

- waiting on the client's VAT number
";

/// Swallows the events a run emits; this file asks the record, not the stream.
#[derive(Debug, Default)]
struct Silent {
    events: Mutex<Vec<Event>>,
}

impl EventSink for Silent {
    fn emit(&self, event: Event) {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(event);
    }
}

/// A data directory, a scaffolded workspace holding the watch runbook, and
/// every store a scheduled run touches.
struct App {
    _dir: TempDir,
    workspace: PathBuf,
    library: PathBuf,
    project_id: String,
    projects: Store,
    agents: AgentStore,
    sessions: SessionStore,
    routines: RoutineStore,
    turns: TurnRegistry,
    grants: GrantStore,
    approvals: ApprovalRegistry,
    audit: AuditLog,
    captures: PathBuf,
    memories: MemoryStore,
    /// Where an ask nobody can answer is filed (PLAN 7.22).
    parked: aegis_lib::ParkedStore,
    /// What is told to somebody who is not at the window.
    notifier: aegis_lib::Quiet,
}

impl App {
    fn new() -> Self {
        let dir = TempDir::new().expect("temp dir");
        let data = dir.path().join("data");
        let workspace = dir.path().join("work");
        std::fs::create_dir_all(&data).expect("data dir");
        std::fs::create_dir_all(&workspace).expect("workspace dir");
        let workspace = dunce::canonicalize(&workspace).expect("canonical workspace");

        let library = data.join(skills::LIBRARY_DIR);
        skills::seed(&library);
        aegis_lib::workspace::scaffold(&workspace).expect("the convention is laid down");

        let skill_dir = workspace
            .join(aegis_lib::workspace::CABINET_DIR)
            .join(skills::LIBRARY_DIR)
            .join(WATCH_SKILL);
        std::fs::create_dir_all(&skill_dir).expect("skill dir");
        std::fs::write(skill_dir.join(skills::SKILL_FILE), WATCH_RUNBOOK).expect("runbook");

        let projects = Store::load(&data);
        let project_id = projects
            .create("Work", &workspace)
            .expect("the project is created")
            .id;

        Self {
            workspace,
            library,
            _dir: dir,
            project_id,
            projects,
            agents: AgentStore::load(&data),
            sessions: SessionStore::load(&data),
            routines: RoutineStore::load(&data),
            turns: TurnRegistry::new(),
            grants: GrantStore::new(),
            approvals: ApprovalRegistry::new(),
            audit: AuditLog::new(&data),
            captures: data.join("captures"),
            memories: MemoryStore::load(&data),
            parked: aegis_lib::ParkedStore::load(&data),
            notifier: aegis_lib::Quiet,
        }
    }

    /// An identity granted the watch runbook and the tools it declares.
    fn watcher(&self) -> Agent {
        self.agents
            .create(&AgentDraft {
                name: "Watcher".to_owned(),
                role: "keeps an eye on what changed".to_owned(),
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
                skills: vec![WATCH_SKILL.to_owned()],
                runs_per_day: 24,
            })
            .expect("the identity is accepted")
    }

    /// Puts a routine on the clock, signed for whatever `grants` says.
    fn routine(&self, agent: &Agent, grants: Vec<Grant>) -> String {
        self.routines
            .create(&RoutineDraft {
                name: "Morning watch".to_owned(),
                project_id: self.project_id.clone(),
                agent_id: agent.id.clone(),
                skill: WATCH_SKILL.to_owned(),
                schedule: Schedule::Every { minutes: 60 },
                grants,
                runs_per_day: 4,
            })
            .expect("the routine is stored")
            .id
    }

    /// Fires one routine the way the scheduler fires it.
    async fn fire(&self, routine_id: &str) {
        let sink = Silent::default();
        let provider = |_: &Agent, _: &str| Box::new(FakeProvider::instant()) as Box<dyn Provider>;
        let host = runner::Host {
            projects: &self.projects,
            routines: &self.routines,
            parked: &self.parked,
            notifier: &self.notifier,
            agents: &self.agents,
            sessions: &self.sessions,
            turns: &self.turns,
            grants: &self.grants,
            approvals: &self.approvals,
            audit: &self.audit,
            sink: &sink,
            self_exe: None,
            captures: &self.captures,
            skills: &self.library,
            memories: &self.memories,
            connectors: aegis_lib::Connectors::none(),
            provider: &provider,
            decision: None,
        };

        runner::fire(&host, routine_id).await;
    }

    /// The window a board is folded from.
    fn window(&self) -> Vec<AuditEntry> {
        self.audit.tail(1000, None).expect("the log is readable")
    }

    /// The project's sessions, and what each of their turns spent.
    ///
    /// The composition `AppState::board` makes; done here by hand because a
    /// test binary has no Tauri application to ask for one.
    fn ledger(&self) -> Vec<trace::SessionLedger> {
        self.sessions
            .list(&self.project_id, &self.turns.lookup())
            .into_iter()
            .map(|session| trace::SessionLedger {
                session_id: session.id.clone(),
                title: session.title.clone(),
                routine: session
                    .scheduled
                    .as_ref()
                    .map(|scheduled| scheduled.routine_name.clone())
                    .unwrap_or_default(),
                handoff: session
                    .delegated
                    .as_ref()
                    .map(|delegated| delegated.handoff_id.clone())
                    .unwrap_or_default(),
                running: matches!(
                    session.state,
                    aegis_lib::SessionState::Running | aegis_lib::SessionState::AwaitingApproval
                ),
                turns: self.sessions.costs(&session.id).unwrap_or_default(),
            })
            .collect()
    }

    /// Every run of the project, newest first.
    fn runs(&self) -> Vec<trace::Run> {
        trace::fold(&self.window(), &self.ledger())
    }
}

// ---------------------------------------------------------------------------
// 1. A run that worked
// ---------------------------------------------------------------------------

/// Who ran, what it cost, what it produced — from the log and the session
/// document, with the transcript never opened.
#[tokio::test]
async fn a_run_says_who_ran_it_what_it_spent_and_what_it_left_behind() {
    let app = App::new();
    let agent = app.watcher();
    let routine = app.routine(&agent, vec![Grant::FsWrite]);

    app.fire(&routine).await;

    let runs = app.runs();
    let run = runs
        .iter()
        .find(|run| run.run.kind == trace::RunKind::Routine)
        .expect("the firing is a run");

    // Who.
    assert_eq!(run.run.id, routine);
    assert_eq!(
        run.label, "Morning watch",
        "named, not identified by a uuid"
    );
    assert_eq!(run.agents, [agent.id.as_str()]);
    assert_eq!(run.skill, WATCH_SKILL, "the runbook it fired");
    assert_eq!(run.sessions.len(), 1, "a firing is one session");

    // What it cost. The fake provider counts rather than invents, so the exact
    // number is its business; what this asserts is that the turn was charged at
    // all, and charged to this run rather than to the conversation around it.
    assert!(run.cost.turns >= 1, "a run that reached a model has turns");
    assert!(run.calls >= 2, "at least a skill_run and a skill_return");

    // Why it did not fail.
    assert_eq!(run.status, trace::RunStatus::Done);
    assert!(run.reason.is_empty());
    assert!(
        run.artefacts
            .iter()
            .any(|path| path.contains(WATCH_SKILL) || path.contains("status")),
        "the status file it wrote is named on the run: {:?}",
        run.artefacts
    );
    assert!(
        run.tools.iter().any(|tally| tally.tool == tool::FS_WRITE),
        "the tools it used are tallied: {:?}",
        run.tools
    );
}

/// The replay: the run's own lines, in the order they happened.
#[tokio::test]
async fn a_run_can_be_replayed_from_the_lines_it_wrote() {
    let app = App::new();
    let agent = app.watcher();
    let routine = app.routine(&agent, vec![Grant::FsWrite]);

    app.fire(&routine).await;

    let window = app.window();
    let reference = app
        .runs()
        .into_iter()
        .find(|run| run.run.kind == trace::RunKind::Routine)
        .expect("the firing is a run")
        .run;

    let mut lines: Vec<&AuditEntry> = window
        .iter()
        .filter(|entry| trace::RunRef::of(entry) == reference)
        .collect();
    lines.sort_by(|left, right| left.ts.cmp(&right.ts));

    assert!(!lines.is_empty());
    assert!(
        lines.iter().all(|entry| entry.routine == routine),
        "the reference selects exactly this firing"
    );
    assert_eq!(
        lines.first().map(|entry| entry.tool.as_str()),
        Some(tool::SKILL_RUN),
        "a replay opens with the runbook being opened"
    );
    assert_eq!(
        lines.last().map(|entry| entry.tool.as_str()),
        Some(tool::SKILL_RETURN),
        "and closes with it reporting"
    );
}

// ---------------------------------------------------------------------------
// 2. A run that did not
// ---------------------------------------------------------------------------

/// The other half of the exit condition, and the one that matters at four in
/// the morning: a run nobody watched, that could not do what it was asked, is
/// answerable afterwards without opening its transcript.
#[tokio::test]
async fn a_run_that_was_refused_says_so_and_lands_in_the_blocked_column() {
    let app = App::new();
    let agent = app.watcher();
    // Signed for nothing. The runbook declares `fs_write`; an unattended run
    // that was not signed for one is refused rather than asked (Phase 16).
    let routine = app.routine(&agent, Vec::new());

    app.fire(&routine).await;

    let runs = app.runs();
    let run = runs
        .iter()
        .find(|run| run.run.kind == trace::RunKind::Routine)
        .expect("the firing is a run");

    assert!(
        run.status.is_stuck(),
        "a run that could not do its job is not a run that ran: {:?}",
        run.status
    );
    assert!(
        !run.reason.is_empty(),
        "why it failed has to be on the run itself"
    );
    assert!(
        run.denied >= 1,
        "the refusal is counted: {} denied of {} calls",
        run.denied,
        run.calls
    );

    let board = board::assemble(board::Facts {
        project_id: &app.project_id,
        status: None,
        sessions: &app.sessions.list(&app.project_id, &app.turns.lookup()),
        routines: &[],
        approvals: &[],
        parked: &[],
        runs: runs.clone(),
    });

    assert!(
        board
            .blocked
            .iter()
            .any(|item| item.text == "Morning watch"),
        "the run is on the board, under the column that says nobody is waiting \
         on a person: {:?}",
        board.blocked
    );
    assert!(board.attention.is_empty());
}

// ---------------------------------------------------------------------------
// 3. Both halves of the board
// ---------------------------------------------------------------------------

/// What a person wrote down and what the runtime knows, in the same columns.
#[tokio::test]
async fn the_board_is_the_file_and_the_runtime_together() {
    let app = App::new();
    let agent = app.watcher();
    let routine = app.routine(&agent, vec![Grant::FsWrite]);
    std::fs::write(
        app.workspace.join(aegis_lib::workspace::STATUS_FILE),
        STATUS,
    )
    .expect("the board file is the user's to write");

    app.fire(&routine).await;

    let (path, text) = aegis_lib::workspace::status(&app.workspace).expect("the file is there");
    let board = board::assemble(board::Facts {
        project_id: &app.project_id,
        status: Some((&path.display().to_string(), &text)),
        sessions: &app.sessions.list(&app.project_id, &app.turns.lookup()),
        routines: &[],
        approvals: &[],
        parked: &[],
        runs: app.runs(),
    });

    assert!(board.status_path.ends_with("STATUS.md"));
    assert_eq!(
        board
            .attention
            .iter()
            .map(|item| item.text.as_str())
            .collect::<Vec<_>>(),
        ["Chase the signed quote"],
        "the file's own line, and the placeholder columns stayed silent"
    );
    assert_eq!(
        board
            .blocked
            .iter()
            .map(|item| item.source)
            .collect::<Vec<_>>(),
        [board::Source::Status],
        "nothing the runtime knows is stuck; the file's line still is"
    );
    assert!(
        board.in_flight.is_empty(),
        "the run has finished and the file says nothing is running"
    );
    assert!(
        board.cost.turns >= 1,
        "the project's total is every session it has"
    );
}
