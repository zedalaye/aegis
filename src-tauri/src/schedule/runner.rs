//! Firing a routine: the tick, and the run it opens (PLAN 7.3, Phase 16).
//!
//! Adapts [`schedule`](super)'s rules to the existing machinery: a scheduled
//! run is an ordinary session, [`Turn`] loop, matrix and audit log.
//!
//! * **The clock presses send**: [`tick`] checks due, granted, folder and
//!   budget, then opens a session with one message. Only policy (unattended)
//!   and the audit line (`routine`) know it came from a clock.
//! * **Bounded**: [`RUN_TIMEOUT`] cancels through the Stop token; two silent
//!   runs pause the routine.
//! * **No fan-out**: [`Standing::Own`] with no bus, so `handoff_delegate` is not
//!   offered — nobody signed for other agents' sessions.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tauri::{AppHandle, Manager as _, Runtime};
use tokio::time::Duration;

use crate::agent::event::EventSink;
use crate::agent::provider::Provider;
use crate::agent::turn::{self, Standing, Turn, TurnPlan, Unattended};
use crate::agent::{Event, StopReason, TurnRegistry};
use crate::approval::ApprovalRegistry;
use crate::audit::AuditLog;
use crate::commands::session::WindowSink;
use crate::error::{AppError, AppResult};
use crate::exec_host::ExecHost;
use crate::mcp::Connectors;
use crate::notify::{Note, Notifier};
use crate::park::{Parking, Parks};
use crate::policy::GrantStore;
use crate::skills::{self, Reported};
use crate::state::AppState;
use crate::store::parked::{ParkedAsk, ParkedStore};
use crate::store::routines::{Routine, RoutineStore, RunOutcome, Schedule};
use crate::store::{
    Agent, AgentStore, MemoryStore, Message, Scheduled, SessionState, SessionStore,
};

use super::{Watch, MAX_IN_FLIGHT, TICK};

/// How long one scheduled run may take, enforced through the turn's
/// cancellation token like **Stop**.
pub const RUN_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Everything a scheduled run borrows from the runtime around it.
///
/// Like [`handoff::runner::Host`](crate::handoff::runner::Host), but with the
/// routine and project stores.
pub struct Host<'a> {
    /// Where a routine's project is resolved to a folder.
    pub projects: &'a crate::store::Store,
    /// The ledger: what fired, what it spent, how it ended.
    pub routines: &'a RoutineStore,
    /// Where an ask nobody can answer is filed (PLAN 7.22).
    pub parked: &'a ParkedStore,
    /// Who is told when a run wants something and nobody is at the window.
    pub notifier: &'a dyn Notifier,
    /// Where the routine's identity is looked up.
    pub agents: &'a AgentStore,
    /// Where the run's session is created and its transcript kept.
    pub sessions: &'a SessionStore,
    /// Which sessions are running; the run registers here like any turn.
    pub turns: &'a TurnRegistry,
    /// Live session grants. The routine's standing approvals are seeded here
    /// for the length of the run, and dropped with it.
    pub grants: &'a GrantStore,
    /// Where an approval would wait — and, in an unattended run, never does.
    pub approvals: &'a ApprovalRegistry,
    /// Where its tool calls are recorded, under the routine's id.
    pub audit: &'a AuditLog,
    /// Where its events go, so a run is watchable when somebody is watching.
    pub sink: &'a dyn EventSink,
    /// This application's binary, so `shell_exec` can refuse to run it.
    pub self_exe: Option<&'a Path>,
    /// Where `screen_capture` writes.
    pub captures: &'a Path,
    /// The skill library.
    pub skills: &'a Path,
    /// Where the identity's memories live.
    pub memories: &'a MemoryStore,
    /// The running connectors (Phase 18); a run may call a tool only if the
    /// routine signed for it by name.
    pub connectors: &'a Connectors,
    /// Which provider answers for an identity (PLAN 7.1, *Provider*).
    pub provider: &'a (dyn Fn(&Agent, &str) -> Box<dyn Provider> + Send + Sync),
    /// The decision client (PLAN 7.18), or `None` without a TypeSafe key.
    pub decision: Option<&'a crate::agent::decision::DecisionClient>,
}

/// Which routines are running now, in memory only. Shared by the tick and
/// **Run now**, so a routine never runs twice at once.
#[derive(Debug, Default)]
pub struct Scheduler {
    running: Mutex<HashSet<String>>,
}

impl Scheduler {
    /// A scheduler with nothing in flight.
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes a slot for `routine_id`, or says why there is none.
    pub fn claim(&self, routine_id: &str) -> AppResult<()> {
        let mut running = self.running();

        if running.contains(routine_id) {
            return Err(AppError::Routine {
                field: "run",
                reason: "this routine is already running".to_owned(),
            });
        }
        if running.len() >= MAX_IN_FLIGHT {
            return Err(AppError::Routine {
                field: "run",
                reason: format!(
                    "{MAX_IN_FLIGHT} scheduled runs are already going; this one waits for the \
                     next tick"
                ),
            });
        }

        running.insert(routine_id.to_owned());
        Ok(())
    }

    /// Gives a slot back, however the run ended.
    pub fn release(&self, routine_id: &str) {
        self.running().remove(routine_id);
    }

    /// Whether this routine is running right now.
    pub fn is_running(&self, routine_id: &str) -> bool {
        self.running().contains(routine_id)
    }

    /// Locks the set, recovering from a poisoned mutex.
    fn running(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

// ---------------------------------------------------------------------------
// One run
// ---------------------------------------------------------------------------

/// What a run needs that the routine does not carry: where it runs, as whom,
/// and which runbook.
struct Ready {
    agent: Agent,
    workspace: PathBuf,
    /// Where the project's commands run (PLAN 7.12). `None` is this process.
    exec_host: Option<ExecHost>,
    skill: crate::skills::Skill,
}

/// Whether the day's budget still has to hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Budget {
    /// A fire: the run has not been charged yet, and the ceiling decides.
    Enforced,
    /// A resume (PLAN 7.22): this run was charged when it started.
    Spent,
}

/// Resolves everything a run needs, or says why it cannot happen.
///
/// Re-asks [`inspect`](super::inspect)'s questions at firing time.
fn ready(host: &Host<'_>, routine: &Routine, budget: Budget) -> Result<Ready, String> {
    let agent = host
        .agents
        .get(&routine.agent_id)
        .map_err(|_| "the identity this runs as is no longer on file".to_owned())?;

    let project = host
        .projects
        .get(&routine.project_id)
        .ok()
        .filter(|project| project.workspace_exists)
        .ok_or_else(|| "this project's folder is not there".to_owned())?;
    let exec_host = project.exec_host.clone();
    let workspace = PathBuf::from(project.workspace_path);

    let catalog = skills::catalog(host.skills, Some(&workspace));
    let skill = skills::find(&catalog, &routine.skill)
        .cloned()
        .ok_or_else(|| format!("`{}` is not in the library or the workspace", routine.skill))?;

    // A resume is asked everything a fire is asked except the day's ceilings,
    // which it was charged against when it started: refusing to finish work a
    // person has just approved because the count is spent would strand the
    // half-done tree the park was protecting.
    let problem = match budget {
        Budget::Enforced => super::inspect(
            routine,
            Some(&agent),
            Some(&workspace),
            Some(&skill),
            host.routines.runs_today_for_agent(&routine.agent_id),
        ),
        Budget::Spent => {
            let mut resumed = routine.clone();
            resumed.runs_today = 0;
            super::inspect(&resumed, Some(&agent), Some(&workspace), Some(&skill), 0)
        }
    };
    if let Some(problem) = problem {
        return Err(problem);
    }

    Ok(Ready {
        agent,
        workspace,
        exec_host,
        skill,
    })
}

/// Runs one routine, start to finish, and records what became of it.
///
/// Never errors: failures are a [`RunOutcome::Failed`] on the routine's row. A
/// shut door is refused before any session opens.
pub async fn fire(host: &Host<'_>, routine_id: &str) {
    let routine = match host.routines.get(routine_id) {
        Ok(routine) => routine,
        Err(err) => {
            tracing::warn!(%err, routine_id, "a routine went away before it could run");
            return;
        }
    };

    let ready = match ready(host, &routine, Budget::Enforced) {
        Ok(ready) => ready,
        Err(reason) => {
            tracing::info!(routine = %routine.name, %reason, "a routine did not run");
            finish(host, &routine, "", RunOutcome::Failed, &reason);
            return;
        }
    };

    // The budget is charged before anything is opened, and under the store's
    // own lock (see `RoutineStore::begin_run`), so two ticks cannot each read
    // "one left" and both fire.
    if let Err(err) = host.routines.begin_run(routine_id) {
        tracing::info!(routine = %routine.name, %err, "a routine had no budget left");
        return;
    }

    let session = match host.sessions.create_scheduled(
        &routine.project_id,
        Some(&super::title(&routine)),
        &ready.agent.id,
        Scheduled {
            routine_id: routine.id.clone(),
            routine_name: routine.name.clone(),
            skill: routine.skill.clone(),
        },
    ) {
        Ok(session) => session,
        Err(err) => {
            finish(
                host,
                &routine,
                "",
                RunOutcome::Failed,
                &format!("its session could not be opened: {err}"),
            );
            return;
        }
    };
    // Emitted as it opens rather than when it ends: work the machine does on
    // your behalf should be watchable while it happens, the same as a brief's.
    host.sink
        .emit(Event::SessionUpdated(Box::new(session.clone())));

    // The signed approvals become ordinary session grants for this run only.
    for grant in &routine.grants {
        host.grants.insert(&session.id, grant.clone());
    }

    let outcome = drive(
        host,
        &routine,
        &ready,
        &session.id,
        super::opening(&routine, &ready.skill),
    )
    .await;

    host.grants.clear(&session.id);
    finish(host, &routine, &session.id, outcome.0, &outcome.1);
}

/// Picks a run up where a parked ask left it (PLAN 7.22).
///
/// The same session, a new turn, still unattended: the routine's standing
/// approvals are seeded again, and an *allow once* answer is already in the
/// grant store as a one-shot on that exact call.
pub async fn resume(host: &Host<'_>, ask: &ParkedAsk, decision: crate::approval::Decision) {
    let routine = match host.routines.get(&ask.routine_id) {
        Ok(routine) => routine,
        Err(err) => {
            tracing::warn!(%err, "a parked ask was answered for a routine that is gone");
            return;
        }
    };

    let ready = match ready(host, &routine, Budget::Spent) {
        Ok(ready) => ready,
        Err(reason) => {
            tracing::info!(routine = %routine.name, %reason, "a parked run could not be resumed");
            finish(
                host,
                &routine,
                &ask.session_id,
                RunOutcome::Failed,
                &format!("it could not be resumed: {reason}"),
            );
            return;
        }
    };

    for grant in &routine.grants {
        host.grants.insert(&ask.session_id, grant.clone());
    }

    let outcome = drive(
        host,
        &routine,
        &ready,
        &ask.session_id,
        crate::park::resumption(ask, decision),
    )
    .await;

    host.grants.clear(&ask.session_id);
    finish(host, &routine, &ask.session_id, outcome.0, &outcome.1);
}

/// Opens the turn, runs it under the deadline, and reads the report out.
///
/// `opening` is the one message the run is given: the runbook for a fire, the
/// answer for a resume.
async fn drive(
    host: &Host<'_>,
    routine: &Routine,
    ready: &Ready,
    session_id: &str,
    opening: String,
) -> (RunOutcome, String) {
    let turn_id = uuid::Uuid::new_v4().to_string();

    let cancel = match host.turns.begin(session_id, &turn_id) {
        Ok(token) => token,
        Err(err) => return (RunOutcome::Failed, err.to_string()),
    };

    if let Err(err) =
        host.sessions
            .append(session_id, Message::user(opening), SessionState::Running)
    {
        host.turns.finish(session_id, &turn_id, SessionState::Idle);
        return (
            RunOutcome::Failed,
            format!("its opening message could not be recorded: {err}"),
        );
    }

    // The deadline reaches the turn through its own token, so a run that has
    // run out of time stops the way a stopped one does rather than being
    // abandoned while it keeps working.
    let deadline = cancel.clone();
    let timer = tokio::spawn(async move {
        tokio::time::sleep(RUN_TIMEOUT).await;
        deadline.cancel();
    });

    let reported = Reported::new();
    let parks = Parks::new();
    let parking = Parking {
        store: host.parked,
        notifier: host.notifier,
        project_id: &routine.project_id,
        routine_id: &routine.id,
        routine_name: &routine.name,
        skill: &routine.skill,
        parks: &parks,
    };
    let provider = (host.provider)(&ready.agent, session_id);
    let plan = TurnPlan {
        session_id: session_id.to_owned(),
        turn_id: turn_id.clone(),
        workspace: Some(ready.workspace.clone()),
        exec_host: ready.exec_host.clone(),
    };

    let reason = Turn {
        agent: &ready.agent,
        sessions: host.sessions,
        turns: host.turns,
        grants: host.grants,
        approvals: host.approvals,
        audit: host.audit,
        provider: provider.as_ref(),
        sink: host.sink,
        self_exe: host.self_exe,
        captures: host.captures,
        skills: host.skills,
        memories: host.memories,
        connectors: host.connectors,
        decision: host.decision,
        // No bus: a scheduled run does the work, it does not hand it out. See
        // the module note.
        standing: Standing::Own(None),
        unattended: Some(Unattended {
            routine: &routine.id,
            reported: &reported,
        }),
        parking: Some(&parking),
    }
    .run(&plan, &cancel)
    .await;

    timer.abort();
    let resting = turn::resting_state(reason);
    host.turns.finish(session_id, &turn_id, resting);
    if let Some(summary) = turn::summarize(host.sessions, session_id, resting) {
        host.sink.emit(Event::SessionUpdated(Box::new(summary)));
    }

    let ended = match reported.take() {
        Some(returned) => (outcome_of(&returned.status), returned.summary),
        // The one thing a run owes is a return. Anything else — a turn that
        // stopped talking, a deadline, a provider that failed — is a silence,
        // and two silences running pause the routine (`RoutineStore::end_run`).
        None => (
            RunOutcome::Failed,
            match reason {
                StopReason::Cancelled => {
                    format!(
                        "it was stopped before it returned (runs are cut off after {} minutes)",
                        RUN_TIMEOUT.as_secs() / 60
                    )
                }
                StopReason::Error => "the turn failed before it returned".to_owned(),
                _ => "it finished without calling `skill_return`, so there is no report".to_owned(),
            },
        ),
    };

    // A park outranks whatever else the run said about itself: something is
    // waiting for a person, and that is what the ledger and the board should
    // say (PLAN 7.22). It is an answer, so it never counts as a silence.
    if parks.any() {
        let parked = parks.ids().len();
        let word = if parked == 1 { "call" } else { "calls" };
        return (
            RunOutcome::Parked,
            format!("{parked} {word} parked for you to answer. {}", ended.1)
                .trim_end()
                .to_owned(),
        );
    }
    ended
}

/// The routine's row after a run, written, announced, and — when it wants
/// somebody — notified (PLAN 7.22).
fn finish(host: &Host<'_>, routine: &Routine, session_id: &str, outcome: RunOutcome, detail: &str) {
    match host
        .routines
        .end_run(&routine.id, session_id, outcome, detail)
    {
        Ok(updated) => {
            tracing::info!(
                routine = %updated.name,
                outcome = outcome.as_str(),
                paused = updated.paused,
                "a scheduled run ended"
            );
            notify(host, &updated, outcome, detail);
            host.sink.emit(Event::RoutineUpdated(Box::new(updated)));
        }
        Err(err) => tracing::warn!(%err, routine = %routine.name, "could not record a run"),
    }
}

/// Tells whoever is not at the window that this run wants them.
///
/// A routine that stopped itself, and a run that returned `needs_you`. Not a
/// park, which is notified as it happens; not a `done`, which is the machine
/// doing its job.
fn notify(host: &Host<'_>, routine: &Routine, outcome: RunOutcome, detail: &str) {
    let body = match outcome {
        _ if routine.paused && !routine.paused_reason.is_empty() => {
            format!("This routine stopped itself. {}", routine.paused_reason)
        }
        RunOutcome::NeedsYou => detail.to_owned(),
        _ => return,
    };

    host.notifier
        .post(Note::new(routine.id.clone(), routine.name.clone(), body));
}

/// A returned status, as the ledger records it.
fn outcome_of(status: &str) -> RunOutcome {
    match status {
        "done" => RunOutcome::Done,
        "needs_you" => RunOutcome::NeedsYou,
        // Anything a `skill_return` can carry that is not one of the two above
        // is a `blocked`, which is an answer: the runbook said what it needed
        // and stopped instead of inventing it.
        _ => RunOutcome::Blocked,
    }
}

// ---------------------------------------------------------------------------
// The tick
// ---------------------------------------------------------------------------

/// Starts the scheduler on the async runtime.
///
/// One task woken every [`TICK`], re-reading routines each time, window or not
/// (PLAN 7.2, *Process*).
pub fn spawn<R: Runtime>(app: AppHandle<R>) {
    tauri::async_runtime::spawn(async move {
        tracing::info!(seconds = TICK.as_secs(), "scheduler started");
        loop {
            tokio::time::sleep(TICK).await;
            if app.try_state::<AppState>().is_none() {
                tracing::info!("scheduler stopping: the application is shutting down");
                return;
            }
            tick(&app);
        }
    });
}

/// One pass over the routines.
///
/// Synchronous: it only measures and spawns run tasks.
fn tick<R: Runtime>(app: &AppHandle<R>) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let now = chrono::Utc::now();
    expire_parks(app, &state, now);

    for routine in state.routines().list() {
        if routine.paused || state.scheduler().is_running(&routine.id) {
            continue;
        }

        // The watermark moves only when a change is acted on (or learned, see
        // `schedule::learning`), so changes during a cooldown stay pending.
        let watch = watch_for(&state, &routine);
        if let Some(newest) = watch.as_ref().and_then(super::learning) {
            state.routines().mark_seen(&routine.id, newest);
            continue;
        }
        if !super::due(&routine, now, watch.as_ref()) {
            continue;
        }

        // Measured last, because it is the most expensive of the four and the
        // rarest to fail: a routine that is not due does not need its catalog
        // read.
        if let Some(problem) = state.routine_problem(&routine) {
            tracing::debug!(routine = %routine.name, %problem, "a due routine cannot run");
            continue;
        }

        if state.scheduler().claim(&routine.id).is_err() {
            continue;
        }
        // Before the run, so a tick during it does not see the same change.
        if let Some(newest) = watch.as_ref().and_then(|watch| watch.newest.as_deref()) {
            state.routines().mark_seen(&routine.id, newest);
        }
        start(app.clone(), routine.id.clone());
    }
}

/// Closes the parked asks nobody answered in time (PLAN 7.22).
///
/// On the scheduler's tick because that is the one thing in this process that
/// wakes up whether or not a window is open. The run they belonged to ends as
/// `blocked`: the question was asked and went unanswered, which is an answer.
fn expire_parks<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    now: chrono::DateTime<chrono::Utc>,
) {
    let expired = state.parked().prune(now);
    if expired.is_empty() {
        return;
    }

    let sink = WindowSink::new(app.clone());
    for ask in expired {
        sink.emit(Event::ParkedResolved(crate::agent::event::ParkedResolved {
            id: ask.id.clone(),
            session_id: ask.session_id.clone(),
            answer: "expired".to_owned(),
        }));

        if ask.routine_id.is_empty() {
            continue;
        }
        let detail = format!(
            "it parked `{}` for you and nobody answered within {} days",
            ask.tool,
            crate::store::parked::PARK_TTL_DAYS
        );
        if let Some(updated) =
            state
                .routines()
                .close_parked(&ask.routine_id, &ask.session_id, &detail)
        {
            sink.emit(Event::RoutineUpdated(Box::new(updated)));
        }
    }
}

/// What a [`Schedule::OnChange`] routine's folder looks like, or `None` for a
/// routine that watches a clock.
fn watch_for(state: &AppState, routine: &Routine) -> Option<Watch> {
    let Schedule::OnChange { dir } = &routine.schedule else {
        return None;
    };

    // Same containment as tool calls (PLAN 3.2); outside means not watched.
    let workspace = state.workspace_for_project(&routine.project_id)?;
    let resolved = crate::policy::path::resolve(&workspace, dir).ok()?;
    if !resolved.inside {
        tracing::warn!(routine = %routine.name, "a routine watches a directory outside its workspace");
        return None;
    }

    Some(Watch {
        newest: super::newest_change(&resolved.path),
        seen: state.routines().seen(&routine.id).unwrap_or_default(),
    })
}

/// Spawns one run, and gives its slot back however it ends.
///
/// The caller claims the slot; it is released here on every path.
fn start<R: Runtime>(app: AppHandle<R>, routine_id: String) {
    tauri::async_runtime::spawn(async move {
        // Two statements, and the second one always runs: a slot that leaked
        // because of an early return would be a routine marked as running for
        // the rest of the process's life.
        run_in(&app, &routine_id).await;

        if let Some(state) = app.try_state::<AppState>() {
            state.scheduler().release(&routine_id);
        }
    });
}

/// Spawns the resumption of a parked run, and gives its slot back however it
/// ends (PLAN 7.22).
fn start_resume<R: Runtime>(
    app: AppHandle<R>,
    ask: ParkedAsk,
    decision: crate::approval::Decision,
) {
    tauri::async_runtime::spawn(async move {
        resume_in(&app, &ask, decision).await;

        if let Some(state) = app.try_state::<AppState>() {
            state.scheduler().release(&ask.routine_id);
        }
    });
}

/// Assembles the host from a running application and resumes one parked run.
async fn resume_in<R: Runtime>(
    app: &AppHandle<R>,
    ask: &ParkedAsk,
    decision: crate::approval::Decision,
) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };

    let sink = WindowSink::new(app.clone());
    let notifier = crate::notify::Desktop::new(app.clone(), state.coalescer());
    let provider = |agent: &Agent, session_id: &str| state.provider_for(agent, session_id);
    let decision_client = state.decision_client();
    let host = Host {
        projects: state.store(),
        routines: state.routines(),
        parked: state.parked(),
        notifier: &notifier,
        agents: state.agents(),
        sessions: state.sessions(),
        turns: state.turns(),
        grants: state.grants(),
        approvals: state.approvals(),
        audit: state.audit(),
        sink: &sink,
        self_exe: state.self_exe(),
        captures: state.captures(),
        skills: state.skills(),
        memories: state.memories(),
        connectors: state.connectors(),
        provider: &provider,
        decision: decision_client.as_ref(),
    };

    resume(&host, ask, decision).await;
}

/// Takes the slot a resumed run will need, before its answer is recorded
/// (PLAN 7.22).
///
/// Given back with [`Scheduler::release`] if the answer does not happen, and
/// by [`resume_claimed`] when the run ends.
pub fn claim_resume(state: &AppState, ask: &ParkedAsk) -> AppResult<()> {
    state.scheduler().claim(&ask.routine_id)
}

/// Picks up a run whose slot is already claimed, on somebody's say-so.
///
/// The answer itself is recorded by
/// [`AppState::answer_parked`](crate::AppState::answer_parked); this is the
/// run. Everything after this is events.
pub fn resume_claimed<R: Runtime>(
    app: &AppHandle<R>,
    ask: &ParkedAsk,
    decision: crate::approval::Decision,
) {
    start_resume(app.clone(), ask.clone(), decision);
}

/// Assembles the host from a running application and fires one routine.
///
/// State is looked up from the handle, since a `State` borrow cannot outlive
/// the invocation.
async fn run_in<R: Runtime>(app: &AppHandle<R>, routine_id: &str) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };

    let sink = WindowSink::new(app.clone());
    let notifier = crate::notify::Desktop::new(app.clone(), state.coalescer());
    let provider = |agent: &Agent, session_id: &str| state.provider_for(agent, session_id);
    let decision = state.decision_client();
    let host = Host {
        projects: state.store(),
        routines: state.routines(),
        parked: state.parked(),
        notifier: &notifier,
        agents: state.agents(),
        sessions: state.sessions(),
        turns: state.turns(),
        grants: state.grants(),
        approvals: state.approvals(),
        audit: state.audit(),
        sink: &sink,
        self_exe: state.self_exe(),
        captures: state.captures(),
        skills: state.skills(),
        memories: state.memories(),
        connectors: state.connectors(),
        provider: &provider,
        decision: decision.as_ref(),
    };

    fire(&host, routine_id).await;
}

/// Fires a routine now, on somebody's say-so.
///
/// The clock's own unattended path. Works on a paused routine without
/// resuming it; immediate refusals return, the rest is events.
pub fn run_now<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    routine_id: &str,
) -> AppResult<()> {
    let routine = state.routines().get(routine_id)?;

    if let Some(problem) = state.routine_problem(&routine) {
        return Err(AppError::Routine {
            field: "run",
            reason: problem,
        });
    }

    state.scheduler().claim(&routine.id)?;
    start(app.clone(), routine.id.clone());
    Ok(())
}
