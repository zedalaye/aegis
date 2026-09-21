//! Routines, through the crate's public surface.
//!
//! Phase 16's exit condition (PLAN 7.3), with no window:
//!
//! 1. The door holds (PLAN 7.13).
//! 2. A signed run writes its status file without a dialog and returns `done`.
//! 3. An unsigned write is parked, not asked and not lost, and an answer
//!    picks the run up where it stopped (PLAN 7.22).
//! 4. Budget, pause-after-two-silences and `routine` audit tags hold.
//!
//! Commands need a Tauri app; everything below them runs here on temp files.

use std::path::PathBuf;
use std::sync::Mutex;

use tempfile::TempDir;

use aegis_lib::agent::event::EventSink;
use aegis_lib::policy::tool;
use aegis_lib::schedule::{self, runner};
use aegis_lib::skills;
use aegis_lib::store::routines::{RoutineDraft, RunOutcome, Schedule, RUNS_PER_DAY_MAX};
use aegis_lib::{
    Agent, AgentDraft, AgentStore, ApprovalDecision, ApprovalRegistry, AuditDecision, AuditEntry,
    AuditLog, AuditRecord, Event, FakeProvider, Grant, GrantStore, MemoryStore, ModelEvent,
    Outcome, ParkedAsk, Provider, RoutineStore, SessionStore, StopReason, Store, TurnRegistry,
    DEFAULT_PROVIDER_ID,
};

/// One scripted run: the rounds a provider replays, one per request.
type Rounds = Vec<Vec<ModelEvent>>;

/// The runbook the routine fires.
///
/// A watch skill in the shape `PLAN.md` § 7.6 gives them — file in, status out
/// — declaring the one tool that makes the phase interesting: it writes.
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

/// Collects every event a turn emits, so a test can ask what was *not* emitted.
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

/// Keeps what a run tried to tell somebody who was not at the window.
#[derive(Debug, Default)]
struct Heard {
    notes: Mutex<Vec<aegis_lib::Note>>,
}

impl Heard {
    fn notes(&self) -> Vec<aegis_lib::Note> {
        self.notes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl aegis_lib::Notifier for Heard {
    fn post(&self, note: aegis_lib::Note) {
        self.notes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(note);
    }
}

/// A data directory, a scaffolded workspace holding the watch runbook, and
/// every store a scheduled run touches.
struct App {
    _dir: TempDir,
    workspace: PathBuf,
    library: PathBuf,
    /// The project whose folder the runs happen in.
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
    notifier: Heard,
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

        // A workspace runbook, written the way § 7.6 says one is written: a
        // file in a folder somebody owns.
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
            notifier: Heard::default(),
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

    /// The catalog entry for the watch runbook.
    fn skill(&self) -> skills::Skill {
        let catalog = skills::catalog(&self.library, Some(&self.workspace));
        skills::find(&catalog, WATCH_SKILL)
            .cloned()
            .expect("the runbook is in the catalog")
    }

    /// A draft for a routine that fires the watch runbook every hour.
    fn draft(&self, agent: &Agent, grants: Vec<Grant>) -> RoutineDraft {
        RoutineDraft {
            name: "Morning watch".to_owned(),
            project_id: self.project_id.clone(),
            agent_id: agent.id.clone(),
            skill: WATCH_SKILL.to_owned(),
            schedule: Schedule::Every { minutes: 60 },
            grants,
            runs_per_day: 4,
        }
    }

    /// Puts a `skill_return` for this identity on the audit log.
    ///
    /// The evidence the door reads, written directly (a real run is tested in
    /// [`a_scheduled_run_writes_its_status_without_asking_anyone`]).
    fn witness(&self, agent: &Agent, skill: &str) {
        self.audit.append(&AuditRecord {
            session_id: "earlier",
            agent_id: &agent.id,
            turn_id: "turn-earlier",
            call_id: "call-earlier",
            tool: tool::SKILL_RETURN,
            skill,
            handoff: "",
            routine: "",
            decision: AuditDecision::Auto,
            policy_reason: "a runbook closing itself",
            args: &serde_json::json!({ "status": "done" }),
            outcome: Outcome::Ok,
            duration_ms: 1,
            bytes_in: 0,
            bytes_out: 10,
            error_code: None,
            artifact: None,
        });
    }

    /// What a run borrows, with a provider the test chooses.
    fn host<'a>(
        &'a self,
        sink: &'a Recorder,
        provider: &'a (dyn Fn(&Agent, &str) -> Box<dyn Provider> + Send + Sync),
    ) -> runner::Host<'a> {
        runner::Host {
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
            sink,
            self_exe: None,
            captures: &self.captures,
            skills: &self.library,
            memories: &self.memories,
            connectors: aegis_lib::Connectors::none(),
            provider,
            decision: None,
        }
    }

    /// Fires one routine the way the scheduler fires it.
    ///
    /// Through [`runner::fire`] itself, not a copy.
    async fn fire(&self, routine_id: &str, sink: &Recorder) {
        let provider = |_: &Agent, _: &str| Box::new(FakeProvider::instant()) as Box<dyn Provider>;
        runner::fire(&self.host(sink, &provider), routine_id).await;
    }

    /// Fires one routine with the rounds spelled out.
    ///
    /// An answer to a parked ask matches one exact call (PLAN 7.22), so a run
    /// that is going to be resumed is scripted rather than improvised: a
    /// timestamp in the arguments would be a different question the second
    /// time round, which is true of a real model too.
    async fn fire_scripted(&self, routine_id: &str, sink: &Recorder, rounds: Rounds) {
        let provider = move |_: &Agent, _: &str| {
            Box::new(FakeProvider::scripted(rounds.clone())) as Box<dyn Provider>
        };
        runner::fire(&self.host(sink, &provider), routine_id).await;
    }

    /// Picks a parked run up, the way answering it from the board does.
    async fn resume_scripted(
        &self,
        ask: &ParkedAsk,
        decision: ApprovalDecision,
        sink: &Recorder,
        rounds: Rounds,
    ) {
        let provider = move |_: &Agent, _: &str| {
            Box::new(FakeProvider::scripted(rounds.clone())) as Box<dyn Provider>
        };
        runner::resume(&self.host(sink, &provider), ask, decision).await;
    }

    /// Every audit line, newest first.
    fn audit_lines(&self) -> Vec<AuditEntry> {
        self.audit.tail(100, None).expect("the log is readable")
    }
}

/// The `field` a refusal carries on the wire.
///
/// Read through the serialized shape rather than off the enum, because that is
/// what the form receives: a message beside the input that caused it is the
/// whole reason these refusals name a field at all.
fn field(err: &aegis_lib::AppError) -> Option<String> {
    serde_json::to_value(err)
        .ok()?
        .get("field")?
        .as_str()
        .map(str::to_owned)
}

// ---------------------------------------------------------------------------
// 1. The door
// ---------------------------------------------------------------------------

/// The whole of PLAN 7.13's *Phase 16's door*, one refusal at a time: granted,
/// live, and run under watch — and the last of those is a fact about the past
/// that only the audit log can answer.
#[test]
fn a_routine_may_only_name_a_live_granted_skill_somebody_has_already_watched() {
    let app = App::new();
    let agent = app.watcher();
    let skill = app.skill();
    let draft = app.draft(&agent, Vec::new());

    // Never run: refused, and the refusal says to go and watch one.
    let refused = schedule::check(&draft, &agent, Some(&skill), false).expect_err("not witnessed");
    assert_eq!(field(&refused), Some("skill".to_owned()));
    assert!(
        refused.to_string().contains("under watch"),
        "{refused}: the refusal has to name the step that was skipped"
    );

    // Run under watch: accepted.
    app.witness(&agent, WATCH_SKILL);
    let witnessed = app.audit.witnessed(&agent.id, WATCH_SKILL);
    assert!(witnessed, "the log holds the return this identity made");
    schedule::check(&draft, &agent, Some(&skill), witnessed).expect("the door opens");

    // A runbook nothing has written — which is also what a `PROPOSAL.md` is,
    // since only a `SKILL.md` reaches the catalog. Granted on the identity, so
    // that what is being tested is the missing file and not the allow-list.
    let hopeful = app
        .agents
        .update(
            &agent.id,
            &AgentDraft {
                name: agent.name.clone(),
                role: agent.role.clone(),
                instructions: agent.instructions.clone(),
                provider_id: agent.provider_id.clone(),
                model: agent.model.clone(),
                tools: agent.tools.clone(),
                skills: vec![WATCH_SKILL.to_owned(), "watch.proposed".to_owned()],
                runs_per_day: agent.runs_per_day,
            },
        )
        .expect("the identity is edited");
    let mut proposed = draft.clone();
    proposed.skill = "watch.proposed".to_owned();
    let refused = schedule::check(&proposed, &hopeful, None, true).expect_err("no such runbook");
    assert!(
        refused.to_string().contains("PROPOSAL.md"),
        "{refused}: a proposal is never runnable"
    );

    // A runbook this identity was never granted.
    let stranger = app
        .agents
        .create(&AgentDraft {
            name: "Stranger".to_owned(),
            role: "holds every tool and no runbooks".to_owned(),
            instructions: String::new(),
            provider_id: DEFAULT_PROVIDER_ID.to_owned(),
            model: String::new(),
            tools: vec![tool::FS_WRITE.to_owned()],
            skills: Vec::new(),
            runs_per_day: 24,
        })
        .expect("a second identity");
    let refused =
        schedule::check(&draft, &stranger, Some(&skill), true).expect_err("not granted the skill");
    assert!(
        refused.to_string().contains("does not grant it"),
        "{refused}: writing a routine is not granting a runbook"
    );
}

/// A standing approval is bounded twice: by what the runbook says it calls, and
/// by what the identity holds. Neither is something the routine can widen.
#[test]
fn a_standing_approval_cannot_exceed_the_runbook_or_the_identity() {
    let app = App::new();
    let agent = app.watcher();
    let skill = app.skill();
    app.witness(&agent, WATCH_SKILL);

    // `fs_write` is declared by the runbook and held by the identity.
    let signed = app.draft(&agent, vec![Grant::FsWrite]);
    schedule::check(&signed, &agent, Some(&skill), true).expect("a declared tool may be signed");

    // `shell_exec` is neither.
    let overreaching = app.draft(&agent, vec![Grant::shell("git")]);
    let refused =
        schedule::check(&overreaching, &agent, Some(&skill), true).expect_err("not declared");
    assert_eq!(field(&refused), Some("grants".to_owned()));
    assert!(
        refused.to_string().contains("does not say it calls"),
        "{refused}"
    );
}

// ---------------------------------------------------------------------------
// 2 and 3. The run
// ---------------------------------------------------------------------------

/// The exit condition: a run with nothing watching writes into `.aegis/status/` and
/// closes with a report, and the write went through because a person signed for
/// it when they saved the routine — not because anybody was asked.
#[tokio::test]
async fn a_scheduled_run_writes_its_status_without_asking_anyone() {
    let app = App::new();
    let agent = app.watcher();
    app.witness(&agent, WATCH_SKILL);

    let routine = app
        .routines
        .create(&app.draft(&agent, vec![Grant::FsWrite]))
        .expect("the routine is stored");

    let sink = Recorder::default();
    app.fire(&routine.id, &sink).await;

    let ran = app.routines.get(&routine.id).expect("still on file");
    let last = ran.last.clone().expect("the run is on the row");
    assert_eq!(
        last.outcome,
        RunOutcome::Done,
        "the run has to close with a report; a silence is not an answer ({})",
        last.detail
    );
    assert_eq!(ran.runs_today, 1, "the run was charged to the budget");
    assert!(
        sink.names().contains(&"routine:updated"),
        "the row is announced, because nobody was looking at it: {:?}",
        sink.names()
    );
    assert!(
        app.workspace
            .join(format!(".aegis/status/{WATCH_SKILL}.md"))
            .is_file(),
        "the status file is the whole point of a watch routine"
    );
    assert!(
        !sink.names().contains(&"tool:approval_required"),
        "nobody is watching, so nothing may be put to anyone: {:?}",
        sink.names()
    );

    // Every line the run wrote names the routine, and the ones inside the run
    // name the runbook — which is what makes a night's work budgetable and
    // replayable afterwards (PLAN 7.6, PLAN 7.2 row 10).
    let lines = app.audit_lines();
    let mine: Vec<&AuditEntry> = lines
        .iter()
        .filter(|entry| entry.session_id == last.session_id)
        .collect();
    assert!(!mine.is_empty(), "the run made calls");
    assert!(
        mine.iter().all(|entry| entry.routine == routine.id),
        "every line of a scheduled run carries its routine"
    );
    let wrote = mine
        .iter()
        .find(|entry| entry.tool == tool::FS_WRITE)
        .expect("the write is on the log");
    assert_eq!(wrote.skill, WATCH_SKILL, "the write was inside the run");
    assert_eq!(wrote.decision, AuditDecision::Auto);
    assert!(
        wrote.policy_reason.contains("standing approval"),
        "{}: the log says what allowed it, and no dialog did",
        wrote.policy_reason
    );

    // And that run is itself now evidence: the door would open for it.
    assert!(app.audit.witnessed(&agent.id, WATCH_SKILL));
}

/// Where a scripted run writes its status. Fixed, so the same call twice is
/// the same call.
const STATUS_PATH: &str = ".aegis/status/watch.digest.md";

/// What it writes there.
const STATUS_TEXT: &str = "# watch.digest\n\nNothing has changed.\n";

/// One tool call, as a provider streams it.
fn call(id: &str, name: &str, arguments: serde_json::Value) -> Vec<ModelEvent> {
    vec![
        ModelEvent::ToolCallDelta {
            index: 0,
            id: Some(id.to_owned()),
            name: Some(name.to_owned()),
            args_delta: arguments.to_string(),
            thought_signature: None,
        },
        ModelEvent::Finish {
            reason: StopReason::ToolCalls,
            usage: None,
        },
    ]
}

/// A run that loads the runbook, writes its status and closes itself.
///
/// `id` distinguishes the two runs' call ids; the write's arguments are the
/// same in both, which is the point: an *allow once* answer matches one call.
fn watch_rounds(id: &str, status: &str) -> Rounds {
    vec![
        call(
            &format!("{id}-load"),
            tool::SKILL_RUN,
            serde_json::json!({ "name": WATCH_SKILL }),
        ),
        call(
            &format!("{id}-write"),
            tool::FS_WRITE,
            serde_json::json!({
                "path": STATUS_PATH,
                "content": STATUS_TEXT,
                "create_dirs": true,
            }),
        ),
        call(
            &format!("{id}-return"),
            tool::SKILL_RETURN,
            serde_json::json!({
                "status": status,
                "summary": format!("Ran {WATCH_SKILL} and left the status {status}."),
            }),
        ),
    ]
}

/// The same run, signed for nothing. The write is neither done nor thrown
/// away: it is parked for a person, the run says so, and the ledger records an
/// answer rather than a silence (PLAN 7.22).
#[tokio::test]
async fn an_unsigned_run_parks_its_write_instead_of_losing_it() {
    let app = App::new();
    let agent = app.watcher();
    app.witness(&agent, WATCH_SKILL);

    let routine = app
        .routines
        .create(&app.draft(&agent, Vec::new()))
        .expect("the routine is stored");

    let sink = Recorder::default();
    app.fire(&routine.id, &sink).await;

    let last = app
        .routines
        .get(&routine.id)
        .expect("still on file")
        .last
        .expect("the run is on the row");
    assert_eq!(
        last.outcome,
        RunOutcome::Parked,
        "a run that left a question behind has answered ({})",
        last.detail
    );
    assert!(
        !app.workspace.join(STATUS_PATH).exists()
            && !app
                .workspace
                .join(format!(".aegis/status/{WATCH_SKILL}.md"))
                .exists(),
        "a parked call must not reach the disk"
    );

    // Nobody was asked, because nobody could have answered.
    assert!(
        !sink.names().contains(&"tool:approval_required"),
        "a prompt nobody can see is a turn parked until it times out: {:?}",
        sink.names()
    );
    assert!(
        app.approvals.list(None).is_empty(),
        "nothing is left waiting for an answer"
    );
    assert!(
        sink.names().contains(&"parked:updated"),
        "the board is told the moment something is waiting: {:?}",
        sink.names()
    );

    // The question itself, with everything the dialog would have shown.
    let waiting = app.parked.list(Some(&app.project_id));
    assert_eq!(waiting.len(), 1);
    let ask = &waiting[0];
    assert_eq!(ask.tool, tool::FS_WRITE);
    assert_eq!(ask.routine_id, routine.id);
    assert_eq!(ask.skill, WATCH_SKILL);
    // The narrowest grant that covers the write (PLAN 7.23): its folder.
    assert_eq!(
        ask.grant,
        Some(Grant::write_under(".aegis/status").expect("a prefix")),
        "this one can be signed, for the folder it wanted"
    );
    assert_eq!(ask.session_id, last.session_id);

    // And somebody who is not at the window is told — without the path.
    let notes = app.notifier.notes();
    assert_eq!(notes.len(), 1, "one run, one notification");
    assert_eq!(notes[0].title, routine.name);
    assert!(
        !notes[0].body.contains(".aegis"),
        "{}: a notification never quotes an argument",
        notes[0].body
    );

    let lines = app.audit_lines();
    let parked = lines
        .iter()
        .find(|entry| entry.tool == tool::FS_WRITE)
        .expect("the parked call is on the log");
    assert_eq!(parked.decision, AuditDecision::Deny, "nothing ran");
    assert_eq!(parked.outcome, Outcome::Denied);
    assert_eq!(parked.error_code.as_deref(), Some("E_PARKED"));
    assert!(
        parked.policy_reason.contains("parked"),
        "{}: the reason has to be readable a week later",
        parked.policy_reason
    );
}

/// PLAN 7.22's exit: answering *allow once* resumes the run in its own
/// session, and the write lands exactly once.
#[tokio::test]
async fn an_answer_resumes_the_run_and_the_write_lands_once() {
    let app = App::new();
    let agent = app.watcher();
    app.witness(&agent, WATCH_SKILL);

    let routine = app
        .routines
        .create(&app.draft(&agent, Vec::new()))
        .expect("the routine is stored");

    let sink = Recorder::default();
    app.fire_scripted(&routine.id, &sink, watch_rounds("first", "blocked"))
        .await;

    let ask = app
        .parked
        .list(None)
        .first()
        .cloned()
        .expect("the write is waiting for somebody");
    let status = app.workspace.join(STATUS_PATH);
    assert!(!status.exists(), "nothing has been written yet");

    // What answering it from the board does: the one-shot is minted on that
    // exact call, and the question comes off the list before anything runs.
    app.grants.allow_once(&ask.session_id, &ask.fingerprint);
    let answered = app.parked.take(&ask.id).expect("it was still open");

    app.resume_scripted(
        &answered,
        ApprovalDecision::AllowOnce,
        &sink,
        watch_rounds("second", "done"),
    )
    .await;

    assert_eq!(
        std::fs::read_to_string(&status).expect("the write landed"),
        STATUS_TEXT
    );
    let wrote: Vec<_> = app
        .audit_lines()
        .into_iter()
        .filter(|entry| entry.tool == tool::FS_WRITE && entry.outcome == Outcome::Ok)
        .collect();
    assert_eq!(wrote.len(), 1, "the call a person allowed once ran once");
    assert!(
        wrote[0].policy_reason.contains("parked"),
        "{}: the log says which answer let it through",
        wrote[0].policy_reason
    );

    // The run finished in the session it started in, and the ledger says so.
    let last = app
        .routines
        .get(&routine.id)
        .expect("still on file")
        .last
        .expect("a run on the row");
    assert_eq!(last.outcome, RunOutcome::Done, "{}", last.detail);
    assert_eq!(last.session_id, answered.session_id);
    assert!(app.parked.list(None).is_empty(), "nothing is still waiting");

    // And the one-shot was spent: the same call again would ask again.
    assert!(
        !app.grants
            .take_once(&answered.session_id, &answered.fingerprint),
        "an answer covers one call, not a session"
    );
}

// ---------------------------------------------------------------------------
// 4. The ledger
// ---------------------------------------------------------------------------

/// A budget is spent, not checked: the charge and the decision happen under one
/// lock, so a routine cannot be talked into one more run than it has.
#[test]
fn a_routine_cannot_run_past_its_budget() {
    let app = App::new();
    let agent = app.watcher();
    let routine = app
        .routines
        .create(&app.draft(&agent, Vec::new()))
        .expect("stored");

    for _ in 0..routine.runs_per_day {
        app.routines.begin_run(&routine.id).expect("within budget");
    }
    let refused = app
        .routines
        .begin_run(&routine.id)
        .expect_err("the budget is spent");
    assert!(refused.to_string().contains("runs for today"), "{refused}");

    let spent = app.routines.get(&routine.id).expect("still there");
    assert_eq!(spent.runs_today, routine.runs_per_day);
    assert_eq!(
        app.routines.runs_today_for_agent(&agent.id),
        routine.runs_per_day,
        "the identity's ceiling counts what its routines spent"
    );

    // And the panel says so rather than drawing a routine that will not run.
    let problem = schedule::inspect(
        &spent,
        Some(&agent),
        Some(&app.workspace),
        Some(&app.skill()),
        0,
    )
    .expect("a spent routine has a problem");
    assert!(problem.contains("spent"), "{problem}");

    // A ceiling on the *identity* is a separate answer, and it names the role:
    // a second routine with a budget of its own still cannot fire once the
    // identity has spent its day (`COS.md`: budget per agent *and* per
    // routine).
    let mut second = app.draft(&agent, Vec::new());
    second.name = "Evening watch".to_owned();
    let second = app.routines.create(&second).expect("a second clock");
    let problem = schedule::inspect(
        &second,
        Some(&agent),
        Some(&app.workspace),
        Some(&app.skill()),
        agent.runs_per_day,
    )
    .expect("the identity's ceiling is spent");
    assert!(problem.contains("Watcher"), "{problem}");
}

/// Two runs in a row that never returned pause the routine and say why. A
/// `blocked` is not one of those: the runbook answered.
#[test]
fn two_silences_stop_the_clock_and_a_blocked_does_not() {
    let app = App::new();
    let agent = app.watcher();
    let routine = app
        .routines
        .create(&app.draft(&agent, Vec::new()))
        .expect("stored");

    app.routines.begin_run(&routine.id).expect("first");
    let after_one = app
        .routines
        .end_run(&routine.id, "s1", RunOutcome::Failed, "no report")
        .expect("recorded");
    assert!(!after_one.paused, "one silence is bad luck");

    app.routines.begin_run(&routine.id).expect("second");
    let after_two = app
        .routines
        .end_run(&routine.id, "s2", RunOutcome::Failed, "no report again")
        .expect("recorded");
    assert!(after_two.paused, "two is a pattern a person has to look at");
    assert!(
        after_two.paused_reason.contains("without a report"),
        "{}",
        after_two.paused_reason
    );
    assert!(
        !schedule::due(&after_two, chrono::Utc::now(), None),
        "a paused routine is never due"
    );

    // Restarting it is a person saying they have looked: the reason clears and
    // the count starts again.
    let restarted = app
        .routines
        .set_paused(&routine.id, false)
        .expect("un-paused");
    assert!(!restarted.paused);
    assert!(restarted.paused_reason.is_empty());

    app.routines.begin_run(&routine.id).expect("third");
    let blocked = app
        .routines
        .end_run(&routine.id, "s3", RunOutcome::Blocked, "no mail today")
        .expect("recorded");
    assert!(
        !blocked.paused,
        "a routine that reports it is blocked has done its job"
    );
    assert_eq!(
        blocked.last.expect("a run on the row").outcome,
        RunOutcome::Blocked
    );
}

/// A routine is a record of what somebody wanted, so what makes it unrunnable
/// is reported beside it rather than silently deleting or firing it.
#[test]
fn a_routine_says_what_is_wrong_instead_of_firing() {
    let app = App::new();
    let agent = app.watcher();
    let routine = app
        .routines
        .create(&app.draft(&agent, Vec::new()))
        .expect("stored");

    assert_eq!(
        schedule::inspect(
            &routine,
            Some(&agent),
            Some(&app.workspace),
            Some(&app.skill()),
            0
        ),
        None,
        "nothing is wrong with it yet"
    );

    // The identity is still there; the grant is gone.
    let narrowed = app
        .agents
        .update(
            &agent.id,
            &AgentDraft {
                name: agent.name.clone(),
                role: agent.role.clone(),
                instructions: agent.instructions.clone(),
                provider_id: agent.provider_id.clone(),
                model: agent.model.clone(),
                tools: agent.tools.clone(),
                skills: Vec::new(),
                runs_per_day: agent.runs_per_day,
            },
        )
        .expect("the identity is edited");
    let problem = schedule::inspect(
        &routine,
        Some(&narrowed),
        Some(&app.workspace),
        Some(&app.skill()),
        0,
    )
    .expect("an un-granted runbook is a problem");
    assert!(problem.contains("no longer allowed"), "{problem}");

    // The folder is gone.
    let problem = schedule::inspect(&routine, Some(&agent), None, Some(&app.skill()), 0)
        .expect("no workspace is a problem");
    assert!(problem.contains("folder"), "{problem}");

    // The identity is gone.
    let problem = schedule::inspect(&routine, None, Some(&app.workspace), Some(&app.skill()), 0)
        .expect("no identity is a problem");
    assert!(problem.contains("identity"), "{problem}");
}

/// A project is where a routine's runs happen, so a routine cannot outlive one.
/// An identity is only *named* by a routine, and can be pointed at another —
/// which is why deleting one is refused while a clock still fires as it, and
/// deleting a project takes its routines with it.
#[test]
fn a_routine_outlives_neither_its_project_nor_the_identity_it_names() {
    let app = App::new();
    let agent = app.watcher();
    let routine = app
        .routines
        .create(&app.draft(&agent, Vec::new()))
        .expect("stored");

    assert_eq!(app.routines.count_for_agent(&agent.id), 1);

    // The cascade the command performs when a project is forgotten.
    assert_eq!(
        app.routines
            .delete_for_project(&app.project_id)
            .expect("the cascade runs"),
        1
    );
    assert!(app.routines.get(&routine.id).is_err());
    assert_eq!(app.routines.count_for_agent(&agent.id), 0);
}

/// The store's own refusals: a name somebody can find in a list, an interval
/// that is not a loop, a budget with a ceiling. And no prompt field anywhere —
/// a routine is a runbook's name, which is what "never automate a still-fuzzy
/// workflow" looks like in a document.
#[test]
fn the_store_refuses_a_routine_nobody_could_read_or_survive() {
    let app = App::new();
    let agent = app.watcher();

    let mut fast = app.draft(&agent, Vec::new());
    fast.schedule = Schedule::Every { minutes: 1 };
    let refused = app
        .routines
        .create(&fast)
        .expect_err("a loop, not a routine");
    assert_eq!(field(&refused), Some("schedule".to_owned()));

    let mut greedy = app.draft(&agent, Vec::new());
    greedy.runs_per_day = RUNS_PER_DAY_MAX + 1;
    assert_eq!(
        field(&app.routines.create(&greedy).expect_err("over the ceiling")),
        Some("budget".to_owned())
    );

    let mut escaping = app.draft(&agent, Vec::new());
    escaping.schedule = Schedule::OnChange {
        dir: "../elsewhere".to_owned(),
    };
    assert_eq!(
        field(
            &app.routines
                .create(&escaping)
                .expect_err("outside the workspace")
        ),
        Some("schedule".to_owned())
    );

    app.routines
        .create(&app.draft(&agent, Vec::new()))
        .expect("the first is fine");
    assert_eq!(
        field(
            &app.routines
                .create(&app.draft(&agent, Vec::new()))
                .expect_err("two routines cannot share a name")
        ),
        Some("name".to_owned())
    );
}
