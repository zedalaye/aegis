//! Firing a routine: the tick, and the run it opens (PLAN 7.3, Phase 16).
//!
//! The adapter between [`schedule`](super)'s policy — what is due, what a run
//! is told, what it may do with nobody watching — and the machinery that
//! answers it. The machinery is the one that was already here. A scheduled run
//! is an ordinary session, bound to the routine's identity, driven by the
//! ordinary [`Turn`] loop, judged by the ordinary matrix, written to the
//! ordinary audit log. There is no second agent loop and no privileged path,
//! which is the same sentence Phase 15 wrote about delegated runs and is true
//! here for the same reason.
//!
//! ## Three things worth reading the code for
//!
//! **The clock does not act; it presses send.** [`tick`] measures, and
//! everything it measures is a fact somebody can check: is this routine due, is
//! its skill still granted, is its folder still there, has it any budget left.
//! When all four say yes it opens a session and hands it one message. Nothing
//! about the run afterwards knows it came from a clock except the two things
//! that must — policy, which refuses to ask a question nobody can answer, and
//! the audit log, which records which routine a call belongs to.
//!
//! **A run is bounded from the outside.** [`RUN_TIMEOUT`] cancels the turn
//! through the same token a person's Stop uses, so a routine that hangs is a
//! failed run rather than an occupied slot forever. Two of those in a row and
//! the routine pauses itself, saying so on its row — the escalate-after-two of
//! `COS.md` *Loop*, applied to a clock.
//!
//! **A scheduled run does not start other agents.** It is
//! [`Standing::Own`] with no bus, so `handoff_delegate` is not even offered.
//! That is a boundary rather than an oversight: what a person signed when they
//! saved the routine is one identity running one runbook, and a fan-out under a
//! clock would be several unattended sessions nobody signed for, each holding
//! none of this routine's standing approvals and therefore each returning
//! `blocked`. A Chief of Staff routes when a person is there to read the board
//! (Phase 15); Phase 17's board is where that becomes worth revisiting.

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
use crate::policy::GrantStore;
use crate::skills::{self, Reported};
use crate::state::AppState;
use crate::store::routines::{Routine, RoutineStore, RunOutcome, Schedule};
use crate::store::{
    Agent, AgentStore, MemoryStore, Message, Scheduled, SessionState, SessionStore,
};

use super::{Watch, MAX_IN_FLIGHT, TICK};

/// How long one scheduled run may take before it is stopped.
///
/// Longer than a delegated attempt (`handoff::bus::ATTEMPT_TIMEOUT`, five
/// minutes) because a routine is doing the work rather than waiting for someone
/// else to, and shorter than "until somebody notices" because nobody is going
/// to. It is enforced through the turn's own cancellation token, so a stopped
/// run stops the way a person's **Stop** stops one: mid-call, with the
/// transcript and the audit line of whatever it was doing intact.
pub const RUN_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Everything a scheduled run borrows from the runtime around it.
///
/// The same shape and the same reasoning as [`Turn`] and
/// [`handoff::runner::Host`](crate::handoff::runner::Host): the fields are
/// references to stores that live for the process, and a function taking
/// fourteen of them positionally is a call site nobody can read. It is a second
/// struct rather than a reuse of the handoff one because the two borrow
/// different things — a scheduled run needs the routine ledger and the project
/// list, a delegated one needs neither — and letting them drift apart is
/// cheaper than a shared type that grows the union of both.
pub struct Host<'a> {
    /// Where a routine's project is resolved to a folder.
    pub projects: &'a crate::store::Store,
    /// The ledger: what fired, what it spent, how it ended.
    pub routines: &'a RoutineStore,
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
    /// Which provider answers for an identity (PLAN 7.1, *Provider*).
    pub provider: &'a (dyn Fn(&Agent) -> Box<dyn Provider> + Send + Sync),
}

/// Which routines are running right now.
///
/// In memory and nowhere else, because it is a fact about *this process*: a
/// routine is not "running" after a crash, and a persisted flag saying it was
/// would be the thing that stops a clock forever. Held in
/// [`AppState`](crate::AppState) so the tick and the **Run now** button claim
/// from the same set — pressing the button while the clock is firing the same
/// routine is refused rather than run twice.
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
    ///
    /// Both limits are here rather than at the two call sites, so the button
    /// and the clock cannot disagree about them.
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
    skill: crate::skills::Skill,
}

/// Resolves everything a run needs, or says why it cannot happen.
///
/// The same four questions [`inspect`](super::inspect) asks on the panel, asked
/// again here at the moment of firing, because everything they look at can
/// change between a tick and the run it decided on — and because **Run now**
/// reaches this function without going past a panel at all.
fn ready(host: &Host<'_>, routine: &Routine) -> Result<Ready, String> {
    let agent = host
        .agents
        .get(&routine.agent_id)
        .map_err(|_| "the identity this runs as is no longer on file".to_owned())?;

    let workspace = host
        .projects
        .get(&routine.project_id)
        .ok()
        .filter(|project| project.workspace_exists)
        .map(|project| PathBuf::from(project.workspace_path))
        .ok_or_else(|| "this project's folder is not there".to_owned())?;

    let catalog = skills::catalog(host.skills, Some(&workspace));
    let skill = skills::find(&catalog, &routine.skill)
        .cloned()
        .ok_or_else(|| format!("`{}` is not in the library or the workspace", routine.skill))?;

    if let Some(problem) = super::inspect(
        routine,
        Some(&agent),
        Some(&workspace),
        Some(&skill),
        host.routines.runs_today_for_agent(&routine.agent_id),
    ) {
        return Err(problem);
    }

    Ok(Ready {
        agent,
        workspace,
        skill,
    })
}

/// Runs one routine, start to finish, and records what became of it.
///
/// Never returns an error: everything that can go wrong is a
/// [`RunOutcome::Failed`] on the routine's row with the reason in it, because
/// there is nobody to return an error *to* — that is the whole shape of the
/// phase. The one thing it will not do is fire a routine whose door is shut;
/// that is refused before a session is opened, and refused in words the panel
/// shows.
pub async fn fire(host: &Host<'_>, routine_id: &str) {
    let routine = match host.routines.get(routine_id) {
        Ok(routine) => routine,
        Err(err) => {
            tracing::warn!(%err, routine_id, "a routine went away before it could run");
            return;
        }
    };

    let ready = match ready(host, &routine) {
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
    host.sink.emit(Event::SessionUpdated(session.clone()));

    // What a person signed, made real for exactly this run. They are ordinary
    // session grants — the same values the approval dialog creates — so every
    // check downstream is the check that was already there, and they go when
    // the run does.
    for grant in &routine.grants {
        host.grants.insert(&session.id, grant.clone());
    }

    let outcome = drive(host, &routine, &ready, &session.id).await;

    host.grants.clear(&session.id);
    finish(host, &routine, &session.id, outcome.0, &outcome.1);
}

/// Opens the turn, runs it under the deadline, and reads the report out.
async fn drive(
    host: &Host<'_>,
    routine: &Routine,
    ready: &Ready,
    session_id: &str,
) -> (RunOutcome, String) {
    let turn_id = uuid::Uuid::new_v4().to_string();

    let cancel = match host.turns.begin(session_id, &turn_id) {
        Ok(token) => token,
        Err(err) => return (RunOutcome::Failed, err.to_string()),
    };

    let opening = super::opening(routine, &ready.skill);
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
    let provider = (host.provider)(&ready.agent);
    let plan = TurnPlan {
        session_id: session_id.to_owned(),
        turn_id: turn_id.clone(),
        workspace: Some(ready.workspace.clone()),
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
        // No bus: a scheduled run does the work, it does not hand it out. See
        // the module note.
        standing: Standing::Own(None),
        unattended: Some(Unattended {
            routine: &routine.id,
            reported: &reported,
        }),
    }
    .run(&plan, &cancel)
    .await;

    timer.abort();
    let resting = turn::resting_state(reason);
    host.turns.finish(session_id, &turn_id, resting);
    if let Some(summary) = turn::summarize(host.sessions, session_id, resting) {
        host.sink.emit(Event::SessionUpdated(summary));
    }

    match reported.take() {
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
    }
}

/// The routine's row after a run, written and announced.
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
            host.sink.emit(Event::RoutineUpdated(Box::new(updated)));
        }
        Err(err) => tracing::warn!(%err, routine = %routine.name, "could not record a run"),
    }
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
/// One task for the process, woken every [`TICK`], holding nothing between
/// wakes: the routines are read from the store each time, so a routine edited,
/// paused or deleted in Settings takes effect on the next tick with no
/// invalidation to get wrong.
///
/// It runs whether or not there is a window. That is the point of the phase and
/// of the tray (PLAN 7.2, row *Process*): closing the window hides a surface, it
/// does not stop the machine. Events still go out; a sink with no window drops
/// them and says so at debug level.
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
/// Synchronous on purpose: it measures and spawns, and everything it measures
/// is a lock or a `stat`. The runs themselves are tasks, so a routine that
/// takes ten minutes does not hold up the tick that would have started another.
fn tick<R: Runtime>(app: &AppHandle<R>) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let now = chrono::Utc::now();

    for routine in state.routines().list() {
        if routine.paused || state.scheduler().is_running(&routine.id) {
            continue;
        }

        // The watched folder is looked at before `due` and recorded after,
        // whichever way the answer goes: the first look at a folder full of old
        // files is what teaches the routine where "now" is, and a fire that did
        // not update the watermark would fire again on the next tick.
        let watch = watch_for(&state, &routine);
        let go = super::due(&routine, now, watch.as_ref());
        if let Some(watch) = &watch {
            if let Some(newest) = &watch.newest {
                state.routines().mark_seen(&routine.id, newest);
            }
        }
        if !go {
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
        start(app.clone(), routine.id.clone());
    }
}

/// What a [`Schedule::OnChange`] routine's folder looks like, or `None` for a
/// routine that watches a clock.
fn watch_for(state: &AppState, routine: &Routine) -> Option<Watch> {
    let Schedule::OnChange { dir } = &routine.schedule else {
        return None;
    };

    // Resolved through the same containment check every tool call goes through
    // (PLAN 3.2): a workspace-relative directory that resolves outside — a
    // symlink, a `..` somebody hand-edited into the document — is not watched
    // and is not an error the scheduler can do anything about.
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
/// The slot is claimed by the caller and released here, in one place, so there
/// is no path out of a run that leaves a routine marked as running for the rest
/// of the process's life.
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

/// Assembles the host from a running application and fires one routine.
///
/// The state is looked up from the handle rather than captured, for the reason
/// the turn task does it: a `State<'_, AppState>` borrows an invocation that
/// this outlives.
async fn run_in<R: Runtime>(app: &AppHandle<R>, routine_id: &str) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };

    let sink = WindowSink::new(app.clone());
    let provider = |agent: &Agent| state.provider_for(agent);
    let host = Host {
        projects: state.store(),
        routines: state.routines(),
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
        provider: &provider,
    };

    fire(&host, routine_id).await;
}

/// Fires a routine now, on somebody's say-so.
///
/// Deliberately the same path the clock takes, unattended and all: what **Run
/// now** shows you is exactly what happens at three in the morning, including
/// which calls are refused for want of a standing approval. A paused routine
/// still runs — pressing the button is a decision, and it does not restart the
/// clock.
///
/// The checks that can be answered immediately are answered immediately, so the
/// button can say why it did nothing; everything after that is events.
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
