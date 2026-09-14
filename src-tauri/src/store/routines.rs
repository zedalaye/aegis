//! The routine document: `routines.json` (PLAN 7.3, Phase 16).
//!
//! A routine binds a schedule (or a watched folder) to one skill, identity and
//! project. This is data only; [`schedule`](crate::schedule) fires it.
//!
//! * **No prompt field**: a routine names a runbook, so a fuzzy workflow has
//!   nowhere to hide (`COS.md` *Skills*).
//! * **Standing approvals are stored**: the [`Grant`]s a person signed are all
//!   an unattended run may do beyond reading;
//!   [`schedule::check`](crate::schedule::check) bounds them.
//! * **The daily budget is persisted** as a date plus a count, so a restart
//!   does not reset it.
//! * **Problems are derived** ([`Routine::problem`]), never stored.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use super::{now, quarantine, strip_bom, write_atomic};
use crate::error::{AppError, AppResult};
use crate::policy::Grant;

/// Name of the document under the application-data directory.
const ROUTINES_FILE: &str = "routines.json";

/// Schema version of [`RoutinesFile`].
const SCHEMA_VERSION: u32 = 1;

/// Longest routine name.
const NAME_MAX_CHARS: usize = 48;

/// Shortest interval a routine may fire on: a floor against unattended
/// runaways.
pub const EVERY_MIN_MINUTES: u32 = 5;

/// Longest interval, in minutes: a day. Use [`Schedule::DailyAt`] beyond that.
pub const EVERY_MAX_MINUTES: u32 = 24 * 60;

/// Most runs one routine may make in a day, whatever its schedule says.
pub const RUNS_PER_DAY_MAX: u32 = 96;

/// Consecutive silent runs after which a routine pauses itself — two, like the
/// handoff bus's attempts (Phase 15).
pub const FAILURES_BEFORE_PAUSE: u32 = 2;

// ---------------------------------------------------------------------------
// IPC payloads
// ---------------------------------------------------------------------------

/// When a routine fires. A watched folder is the only trigger; anything
/// domain-specific belongs in connectors (PLAN 7.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum Schedule {
    /// Every so many minutes, counted from the last run (no backlog after
    /// sleep).
    Every {
        /// Minutes between runs, from [`EVERY_MIN_MINUTES`] to
        /// [`EVERY_MAX_MINUTES`].
        minutes: u32,
    },
    /// Once a day at a local wall-clock time, stable across daylight saving.
    DailyAt {
        /// Hour of the local day, 0–23.
        hour: u32,
        /// Minute, 0–59.
        minute: u32,
    },
    /// When anything under a workspace directory changes. Polled on the
    /// scheduler tick by comparing the newest modification time.
    OnChange {
        /// The directory, relative to the workspace root: `briefs`, `inbox`.
        dir: String,
    },
}

impl Schedule {
    /// The schedule in the words the routine list shows.
    pub fn label(&self) -> String {
        match self {
            Self::Every { minutes } if *minutes >= 60 && *minutes % 60 == 0 => match minutes / 60 {
                1 => "every hour".to_owned(),
                hours => format!("every {hours} hours"),
            },
            Self::Every { minutes } => format!("every {minutes} minutes"),
            Self::DailyAt { hour, minute } => format!("daily at {hour:02}:{minute:02}"),
            Self::OnChange { dir } => format!("when `{dir}` changes"),
        }
    }
}

/// How a run ended: the three `skill_return` statuses, or no return at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum RunOutcome {
    /// The runbook returned `done`.
    Done,
    /// It returned `blocked`: something it needed was not there.
    Blocked,
    /// It returned `needs_you`: a person has to decide.
    NeedsYou,
    /// It ended without returning at all, or it never started.
    Failed,
}

impl RunOutcome {
    /// Whether this counts against [`FAILURES_BEFORE_PAUSE`]: only silence;
    /// `blocked` is a valid answer.
    pub const fn is_failure(self) -> bool {
        matches!(self, Self::Failed)
    }

    /// The wire word, shared with the UI.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::NeedsYou => "needs_you",
            Self::Failed => "failed",
        }
    }
}

/// What became of the last run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct LastRun {
    /// When it started, RFC3339 UTC; intervals count from here.
    pub at: String,
    /// The session it ran in, so the transcript is one click away.
    pub session_id: String,
    /// How it ended.
    pub outcome: RunOutcome,
    /// The run's own words: a return's summary, or what went wrong.
    pub detail: String,
}

/// A routine, as the UI and the scheduler see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct Routine {
    /// UUID v4.
    pub id: String,
    /// Display name: "Morning watch".
    pub name: String,
    /// The project whose workspace it runs in.
    pub project_id: String,
    /// The identity it runs as.
    pub agent_id: String,
    /// The skill it fires — a live `SKILL.md`, never a chat and never a
    /// proposal (PLAN 7.13, *Phase 16's door*).
    pub skill: String,
    /// When it fires.
    pub schedule: Schedule,
    /// What its runs may do with nobody there to ask.
    pub grants: Vec<Grant>,
    /// Most runs it may make in one day.
    pub runs_per_day: u32,
    /// How many it has made today.
    pub runs_today: u32,
    /// Whether the clock is stopped.
    pub paused: bool,
    /// Why it was stopped, when the scheduler stopped it rather than a person.
    pub paused_reason: String,
    /// When it started counting: set on create, schedule change and un-pause.
    /// Earlier windows never fire ([`crate::schedule::due`]).
    pub armed_at: String,
    /// What became of the last run, if it has run.
    pub last: Option<LastRun>,
    /// Why this routine cannot fire now; derived by
    /// [`crate::schedule::inspect`], never stored.
    pub problem: Option<String>,
    /// RFC3339, UTC.
    pub created_at: String,
    /// RFC3339, UTC.
    pub updated_at: String,
}

/// What a create or an update carries.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct RoutineDraft {
    /// Display name.
    pub name: String,
    /// The project whose workspace it runs in.
    pub project_id: String,
    /// The identity it runs as.
    pub agent_id: String,
    /// The skill it fires.
    pub skill: String,
    /// When it fires.
    pub schedule: Schedule,
    /// What its runs may do unattended. May be empty — a routine that only
    /// reads is the safest thing this document can hold.
    pub grants: Vec<Grant>,
    /// Most runs a day. Capped at [`RUNS_PER_DAY_MAX`].
    pub runs_per_day: u32,
}

// ---------------------------------------------------------------------------
// On-disk shapes
// ---------------------------------------------------------------------------

/// The document itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RoutinesFile {
    version: u32,
    routines: Vec<StoredRoutine>,
}

/// A routine as persisted: not [`Routine`], so `problem` cannot be stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredRoutine {
    id: String,
    name: String,
    project_id: String,
    agent_id: String,
    skill: String,
    schedule: Schedule,
    #[serde(default)]
    grants: Vec<Grant>,
    runs_per_day: u32,
    /// The UTC date the counter below belongs to, `2026-09-01`.
    #[serde(default)]
    day: String,
    #[serde(default)]
    runs_today: u32,
    #[serde(default)]
    paused: bool,
    #[serde(default)]
    paused_reason: String,
    /// Consecutive runs that ended without a report.
    #[serde(default)]
    failures: u32,
    armed_at: String,
    /// The newest modification time seen under a [`Schedule::OnChange`]
    /// directory, RFC3339 UTC. Empty until the first look, which only records.
    #[serde(default)]
    seen: String,
    #[serde(default)]
    last: Option<LastRun>,
    created_at: String,
    updated_at: String,
}

impl StoredRoutine {
    /// The routine as the UI sees it, with nothing measured yet.
    fn to_routine(&self, today: &str) -> Routine {
        Routine {
            id: self.id.clone(),
            name: self.name.clone(),
            project_id: self.project_id.clone(),
            agent_id: self.agent_id.clone(),
            skill: self.skill.clone(),
            schedule: self.schedule.clone(),
            grants: self.grants.clone(),
            runs_per_day: self.runs_per_day,
            // Reset on read too: yesterday's count is not today's.
            runs_today: if self.day == today {
                self.runs_today
            } else {
                0
            },
            paused: self.paused,
            paused_reason: self.paused_reason.clone(),
            armed_at: self.armed_at.clone(),
            last: self.last.clone(),
            problem: None,
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// The routine store: one mutex over the list, written out on every mutation.
#[derive(Debug)]
pub struct RoutineStore {
    path: PathBuf,
    routines: Mutex<Vec<StoredRoutine>>,
}

impl RoutineStore {
    /// Loads the store from `data_dir`. Never fails: a damaged document starts
    /// empty.
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(ROUTINES_FILE);

        let routines = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<RoutinesFile>(strip_bom(&bytes)) {
                Ok(file) if file.version == SCHEMA_VERSION => {
                    tracing::info!(count = file.routines.len(), "routine store loaded");
                    file.routines
                }
                Ok(file) => {
                    tracing::error!(
                        found = file.version,
                        expected = SCHEMA_VERSION,
                        "unknown routine store version"
                    );
                    quarantine(&path);
                    Vec::new()
                }
                Err(err) => {
                    tracing::error!(%err, "routine store is not readable JSON");
                    quarantine(&path);
                    Vec::new()
                }
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                tracing::info!("no routine store yet; nothing is on a clock");
                Vec::new()
            }
            Err(err) => {
                tracing::error!(%err, "could not read the routine store");
                Vec::new()
            }
        };

        Self {
            path,
            routines: Mutex::new(routines),
        }
    }

    /// Locks the list, recovering from a poisoned mutex.
    fn routines(&self) -> MutexGuard<'_, Vec<StoredRoutine>> {
        self.routines
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Every routine, by name.
    pub fn list(&self) -> Vec<Routine> {
        let today = today();
        let routines = self.routines();

        let mut out: Vec<Routine> = routines
            .iter()
            .map(|stored| stored.to_routine(&today))
            .collect();
        out.sort_by_key(|routine| routine.name.to_lowercase());
        out
    }

    /// One routine by id.
    pub fn get(&self, id: &str) -> AppResult<Routine> {
        let today = today();
        let routines = self.routines();
        Ok(Self::find(&routines, id)?.to_routine(&today))
    }

    /// How many routines name this identity, for
    /// [`AppState::delete_agent`](crate::AppState::delete_agent).
    pub fn count_for_agent(&self, agent_id: &str) -> usize {
        self.routines()
            .iter()
            .filter(|routine| routine.agent_id == agent_id)
            .count()
    }

    /// Creates a routine, checking shape only; the door is
    /// [`schedule::check`](crate::schedule::check).
    pub fn create(&self, draft: &RoutineDraft) -> AppResult<Routine> {
        let mut routines = self.routines();
        let valid = Valid::check(draft, &routines, None)?;

        let today = today();
        let stamp = now();
        let stored = StoredRoutine {
            id: Uuid::new_v4().to_string(),
            name: valid.name,
            project_id: valid.project_id,
            agent_id: valid.agent_id,
            skill: valid.skill,
            schedule: valid.schedule,
            grants: valid.grants,
            runs_per_day: valid.runs_per_day,
            day: today.clone(),
            runs_today: 0,
            paused: false,
            paused_reason: String::new(),
            failures: 0,
            armed_at: stamp.clone(),
            seen: String::new(),
            last: None,
            created_at: stamp.clone(),
            updated_at: stamp,
        };
        let created = stored.to_routine(&today);

        routines.push(stored);
        self.save(&routines)?;

        tracing::info!(id = %created.id, name = %created.name, "routine created");
        Ok(created)
    }

    /// Replaces a routine's fields, re-arming only if the schedule changed and
    /// keeping today's spend.
    pub fn update(&self, id: &str, draft: &RoutineDraft) -> AppResult<Routine> {
        let mut routines = self.routines();
        let valid = Valid::check(draft, &routines, Some(id))?;

        let today = today();
        let stamp = now();
        let stored = Self::find_mut(&mut routines, id)?;
        let rearm = stored.schedule != valid.schedule;

        stored.name = valid.name;
        stored.project_id = valid.project_id;
        stored.agent_id = valid.agent_id;
        stored.skill = valid.skill;
        stored.schedule = valid.schedule;
        stored.grants = valid.grants;
        stored.runs_per_day = valid.runs_per_day;
        stored.updated_at = stamp.clone();
        if rearm {
            stored.armed_at = stamp;
            stored.seen = String::new();
        }
        let updated = stored.to_routine(&today);

        self.save(&routines)?;
        tracing::info!(id, name = %updated.name, rearmed = rearm, "routine updated");
        Ok(updated)
    }

    /// Pauses or resumes; resuming re-arms, so no missed window fires
    /// ([`crate::schedule::due`]).
    pub fn set_paused(&self, id: &str, paused: bool) -> AppResult<Routine> {
        let today = today();
        let mut routines = self.routines();
        let stored = Self::find_mut(&mut routines, id)?;

        stored.paused = paused;
        stored.paused_reason = String::new();
        stored.updated_at = now();
        if !paused {
            stored.armed_at = now();
            stored.failures = 0;
        }
        let updated = stored.to_routine(&today);

        self.save(&routines)?;
        tracing::info!(id, paused, "routine clock changed");
        Ok(updated)
    }

    /// Deletes a routine.
    pub fn delete(&self, id: &str) -> AppResult<()> {
        let mut routines = self.routines();

        let before = routines.len();
        routines.retain(|routine| routine.id != id);
        if routines.len() == before {
            return Err(AppError::RoutineNotFound { id: id.to_owned() });
        }

        self.save(&routines)?;
        tracing::info!(id, "routine deleted");
        Ok(())
    }

    /// Deletes every routine of a forgotten project, returning how many went.
    pub fn delete_for_project(&self, project_id: &str) -> AppResult<usize> {
        let mut routines = self.routines();

        let before = routines.len();
        routines.retain(|routine| routine.project_id != project_id);
        let removed = before - routines.len();
        if removed == 0 {
            return Ok(0);
        }

        self.save(&routines)?;
        tracing::info!(project_id, removed, "routines deleted with their project");
        Ok(removed)
    }

    /// Charges one run and stamps the routine as running, failing when the
    /// budget is spent. Check and spend share one lock, before any session
    /// opens; [`RoutineStore::end_run`] adds the session.
    pub fn begin_run(&self, id: &str) -> AppResult<Routine> {
        let today = today();
        let mut routines = self.routines();
        let stored = Self::find_mut(&mut routines, id)?;

        if stored.day != today {
            stored.day = today.clone();
            stored.runs_today = 0;
        }
        if stored.runs_today >= stored.runs_per_day {
            return Err(AppError::Routine {
                field: "budget",
                reason: format!(
                    "`{}` has used its {} runs for today",
                    stored.name, stored.runs_per_day
                ),
            });
        }

        stored.runs_today += 1;
        stored.last = Some(LastRun {
            at: now(),
            session_id: String::new(),
            // Overwritten by `end_run`. A run recorded as anything else while
            // it is still going would be a status nobody measured.
            outcome: RunOutcome::Failed,
            detail: "running".to_owned(),
        });
        let updated = stored.to_routine(&today);

        self.save(&routines)?;
        Ok(updated)
    }

    /// Records how a run ended, pausing after the second silence in a row.
    /// `session_id` is empty for a run that failed before opening one.
    pub fn end_run(
        &self,
        id: &str,
        session_id: &str,
        outcome: RunOutcome,
        detail: &str,
    ) -> AppResult<Routine> {
        let today = today();
        let mut routines = self.routines();
        let stored = Self::find_mut(&mut routines, id)?;

        match stored.last.as_mut() {
            Some(last) => {
                if !session_id.is_empty() {
                    last.session_id = session_id.to_owned();
                }
                last.outcome = outcome;
                last.detail = detail.to_owned();
            }
            // A run refused before it was ever charged: the routine has no
            // stamp to correct, and what happened is still worth a row.
            None => {
                stored.last = Some(LastRun {
                    at: now(),
                    session_id: session_id.to_owned(),
                    outcome,
                    detail: detail.to_owned(),
                });
            }
        }

        if outcome.is_failure() {
            stored.failures += 1;
            if stored.failures >= FAILURES_BEFORE_PAUSE && !stored.paused {
                stored.paused = true;
                stored.paused_reason = format!(
                    "{} runs in a row ended without a report. The last one: {detail}",
                    stored.failures
                );
                tracing::warn!(id, "a routine paused itself after repeated silences");
            }
        } else {
            stored.failures = 0;
        }

        let updated = stored.to_routine(&today);
        self.save(&routines)?;
        Ok(updated)
    }

    /// The watermark a [`Schedule::OnChange`] routine last saw.
    pub fn seen(&self, id: &str) -> Option<String> {
        self.routines()
            .iter()
            .find(|routine| routine.id == id)
            .map(|routine| routine.seen.clone())
    }

    /// Records a new watermark for a [`Schedule::OnChange`] routine. Best
    /// effort.
    pub fn mark_seen(&self, id: &str, stamp: &str) {
        let mut routines = self.routines();
        let Ok(stored) = Self::find_mut(&mut routines, id) else {
            return;
        };
        if stored.seen == stamp {
            return;
        }
        stored.seen = stamp.to_owned();
        if let Err(err) = self.save(&routines) {
            tracing::warn!(%err, id, "could not record what a routine has seen");
        }
    }

    /// Runs this identity's routines made today, summed from their ledgers
    /// (the per-agent budget, `COS.md`).
    pub fn runs_today_for_agent(&self, agent_id: &str) -> u32 {
        let today = today();
        self.routines()
            .iter()
            .filter(|routine| routine.agent_id == agent_id && routine.day == today)
            .map(|routine| routine.runs_today)
            .sum()
    }

    /// Looks a routine up, or reports that the caller's list is stale.
    fn find<'a>(routines: &'a [StoredRoutine], id: &str) -> AppResult<&'a StoredRoutine> {
        routines
            .iter()
            .find(|routine| routine.id == id)
            .ok_or_else(|| AppError::RoutineNotFound { id: id.to_owned() })
    }

    /// [`RoutineStore::find`], mutably.
    fn find_mut<'a>(
        routines: &'a mut [StoredRoutine],
        id: &str,
    ) -> AppResult<&'a mut StoredRoutine> {
        routines
            .iter_mut()
            .find(|routine| routine.id == id)
            .ok_or_else(|| AppError::RoutineNotFound { id: id.to_owned() })
    }

    /// Serializes the list and replaces the document atomically.
    fn save(&self, routines: &[StoredRoutine]) -> AppResult<()> {
        let file = RoutinesFile {
            version: SCHEMA_VERSION,
            routines: routines.to_vec(),
        };
        let bytes = serde_json::to_vec_pretty(&file).map_err(|err| AppError::Store {
            action: "serialize",
            source: io::Error::other(err),
        })?;

        write_atomic(&self.path, &bytes).map_err(|err| {
            tracing::error!(%err, path = %self.path.display(), "could not write the routine store");
            AppError::Store {
                action: "save",
                source: err,
            }
        })
    }

    /// Where the document lives.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Today, UTC, as `2026-09-01` — matching the stamps, even if the budget then
/// resets mid-evening locally.
fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// A draft that has been checked, with every field in the form it is stored in.
struct Valid {
    name: String,
    project_id: String,
    agent_id: String,
    skill: String,
    schedule: Schedule,
    grants: Vec<Grant>,
    runs_per_day: u32,
}

impl Valid {
    /// Checks a draft's shape (name, interval, budget) against the routines on
    /// file; permissions are checked a layer up.
    fn check(
        draft: &RoutineDraft,
        routines: &[StoredRoutine],
        editing: Option<&str>,
    ) -> AppResult<Self> {
        let name = draft.name.trim();
        if name.is_empty() {
            return Err(AppError::Routine {
                field: "name",
                reason: "a routine needs a name — \"Morning watch\"".to_owned(),
            });
        }
        if name.chars().count() > NAME_MAX_CHARS {
            return Err(AppError::Routine {
                field: "name",
                reason: format!("keep it under {NAME_MAX_CHARS} characters"),
            });
        }
        let taken = routines
            .iter()
            .filter(|routine| Some(routine.id.as_str()) != editing)
            .any(|routine| routine.name.eq_ignore_ascii_case(name));
        if taken {
            return Err(AppError::Routine {
                field: "name",
                reason: format!("`{name}` is already a routine — pick another name"),
            });
        }

        let project_id = draft.project_id.trim();
        if project_id.is_empty() {
            return Err(AppError::Routine {
                field: "project",
                reason: "a routine runs in a project's workspace; name one".to_owned(),
            });
        }

        let agent_id = draft.agent_id.trim();
        if agent_id.is_empty() {
            return Err(AppError::Routine {
                field: "identity",
                reason: "a routine runs as an identity; name one".to_owned(),
            });
        }

        let skill = draft.skill.trim();
        if skill.is_empty() {
            return Err(AppError::Routine {
                field: "skill",
                reason: "a routine fires a skill — a runbook, never a message. Grant one to this \
                         identity first"
                    .to_owned(),
            });
        }

        let schedule = check_schedule(&draft.schedule)?;

        if draft.runs_per_day == 0 {
            return Err(AppError::Routine {
                field: "budget",
                reason: "a routine allowed no runs a day is a routine that is paused — pause it \
                         instead, so the reason is on the record"
                    .to_owned(),
            });
        }
        if draft.runs_per_day > RUNS_PER_DAY_MAX {
            return Err(AppError::Routine {
                field: "budget",
                reason: format!(
                    "at most {RUNS_PER_DAY_MAX} runs a day. A ceiling nobody is watching is what \
                     the field is for"
                ),
            });
        }

        // Sorted and deduplicated, so two routines signed for the same things
        // hold the same list and a row reads the same way twice.
        let mut grants = draft.grants.clone();
        grants.sort();
        grants.dedup();

        Ok(Self {
            name: name.to_owned(),
            project_id: project_id.to_owned(),
            agent_id: agent_id.to_owned(),
            skill: skill.to_owned(),
            schedule,
            grants,
            runs_per_day: draft.runs_per_day,
        })
    }
}

/// Checks a schedule, and normalizes the one field that can be spelled several
/// ways.
fn check_schedule(schedule: &Schedule) -> AppResult<Schedule> {
    match schedule {
        Schedule::Every { minutes } => {
            if *minutes < EVERY_MIN_MINUTES || *minutes > EVERY_MAX_MINUTES {
                return Err(AppError::Routine {
                    field: "schedule",
                    reason: format!(
                        "an interval is between {EVERY_MIN_MINUTES} minutes and a day. Under \
                         that it is a loop, not a routine"
                    ),
                });
            }
            Ok(Schedule::Every { minutes: *minutes })
        }
        Schedule::DailyAt { hour, minute } => {
            if *hour > 23 || *minute > 59 {
                return Err(AppError::Routine {
                    field: "schedule",
                    reason: "a time of day is between 00:00 and 23:59".to_owned(),
                });
            }
            Ok(Schedule::DailyAt {
                hour: *hour,
                minute: *minute,
            })
        }
        Schedule::OnChange { dir } => {
            let dir = dir.trim().trim_matches(['/', '\\']).trim();
            if dir.is_empty() {
                return Err(AppError::Routine {
                    field: "schedule",
                    reason: "name a directory inside the workspace to watch — `briefs`, `inbox`"
                        .to_owned(),
                });
            }
            // Containment is checked at fire time; here only `..` is refused.
            if dir.split(['/', '\\']).any(|part| part == "..") {
                return Err(AppError::Routine {
                    field: "schedule",
                    reason: "the directory is inside the workspace; `..` has no meaning here"
                        .to_owned(),
                });
            }
            Ok(Schedule::OnChange {
                dir: dir.to_owned(),
            })
        }
    }
}
