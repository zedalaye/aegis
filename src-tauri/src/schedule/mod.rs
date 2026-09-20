//! Routines: a clock on a skill (PLAN 7.3, Phase 16).
//!
//! The scheduler's rules — which routine may exist, when one is due, what its
//! run is told — kept pure so they test without an app, clock or model.
//! [`runner`] opens and drives the session.
//!
//! * **The door** ([`check`]): a live skill, granted to the identity, and
//!   already carried to a `skill_return` by it according to the audit log
//!   (PLAN 7.13).
//! * **Always unattended**, `Run now` included: every *ask* is refused
//!   ([`PolicyCtx::unattended`](crate::policy::PolicyCtx::unattended)), so a
//!   run does only what its signed [`Grant`](crate::policy::Grant)s allow,
//!   which [`check`] bounds by the runbook's tools and the allow-list.
//! * **Not a chat**: the opening message is written here ([`opening`]); two
//!   silent runs in a row pause the routine.

pub mod runner;

use std::path::Path;

use chrono::{DateTime, Local, SecondsFormat, TimeZone as _, Utc};

use crate::error::{AppError, AppResult};
use crate::policy::Grant;
use crate::skills::Skill;
use crate::store::routines::{Routine, RoutineDraft, Schedule, EVERY_MIN_MINUTES};
use crate::store::Agent;

/// How often the scheduler looks at its routines. Schedules ask "is it time
/// yet", so a late tick is late, never missed.
pub const TICK: std::time::Duration = std::time::Duration::from_secs(30);

/// Most scheduled runs in flight at once, across all routines; the rest wait
/// for a later tick.
pub const MAX_IN_FLIGHT: usize = 2;

/// How deep under a watched directory a change is looked for.
const WATCH_DEPTH: usize = 3;

/// How many entries a watched-directory walk visits before answering with the
/// newest seen.
const WATCH_ENTRIES: usize = 2_000;

/// What a watched directory looked like, this tick and last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watch {
    /// The newest modification time under the directory, RFC3339 UTC, or `None`
    /// when the directory is missing or empty.
    pub newest: Option<String>,
    /// The newest this routine had already seen. Empty before the first look.
    pub seen: String,
}

// ---------------------------------------------------------------------------
// The door
// ---------------------------------------------------------------------------

/// Whether this routine may be saved (PLAN 7.13, *Phase 16's door*).
///
/// `skill` is the catalog entry for the draft's name; `witnessed` is whether
/// the audit log holds this identity's `skill_return` for it. Every refusal
/// names the fix.
pub fn check(
    draft: &RoutineDraft,
    agent: &Agent,
    skill: Option<&Skill>,
    witnessed: bool,
) -> AppResult<()> {
    let name = draft.skill.trim();

    // Granted first, because it is the answer that does not depend on a file
    // being on disk: an identity that was never given the runbook cannot run it
    // whether or not somebody has written one.
    if !agent.allows_skill(name) {
        return Err(AppError::Routine {
            field: "skill",
            reason: format!(
                "`{}` is not allowed to run `{name}`. Tick it under Skills on that identity in \
                 Settings first — putting a runbook on a clock does not grant it",
                agent.name
            ),
        });
    }

    let Some(skill) = skill else {
        return Err(AppError::Routine {
            field: "skill",
            reason: format!(
                "there is no `{name}` in the library or in this project's workspace. A routine \
                 fires a runbook that exists; only a `SKILL.md` is one, and a `PROPOSAL.md` is \
                 not"
            ),
        });
    };

    if let Some(problem) = &skill.problem {
        return Err(AppError::Routine {
            field: "skill",
            reason: format!(
                "`{name}` is not a runbook this build can follow: {problem}. Fix the file, then \
                 put it on a clock"
            ),
        });
    }

    // Fail closed at the door too, so the form explains it (PLAN 7.6).
    if let Some(missing) = skill
        .tools
        .iter()
        .find(|wanted| !agent.tools.iter().any(|held| held == *wanted))
    {
        return Err(AppError::Routine {
            field: "skill",
            reason: format!(
                "`{name}` calls `{missing}`, which `{}` does not hold, so every run would stop \
                 before its first step",
                agent.name
            ),
        });
    }

    if !witnessed {
        return Err(AppError::Routine {
            field: "skill",
            reason: format!(
                "`{name}` has not been run under watch yet. Open a session as `{}`, run it once \
                 and watch what it does — a routine is the last step of promoting a procedure, \
                 not the first",
                agent.name
            ),
        });
    }

    if agent.runs_per_day == 0 {
        return Err(AppError::Routine {
            field: "identity",
            reason: format!(
                "`{}` is allowed no scheduled runs a day, so this routine could never fire. \
                 Raise that ceiling on the identity in Settings, or pick another one",
                agent.name
            ),
        });
    }

    for grant in &draft.grants {
        // No routine may amend the world (PLAN 7.2); catches a hand-edited
        // `routines.json`.
        if matches!(grant, Grant::WorldAmend) {
            return Err(AppError::Routine {
                field: "grants",
                reason: "amending `world/` is a human decision, and nobody is watching a \
                         scheduled run. A routine can read the world; changing it is yours, in \
                         a session"
                    .to_owned(),
            });
        }

        let tool = grant.tool();
        if !skill.tools.iter().any(|declared| declared == tool) {
            return Err(AppError::Routine {
                field: "grants",
                reason: format!(
                    "`{name}` does not say it calls `{tool}`, so a standing approval for it \
                     would cover something the runbook never described. Add it under *Inputs \
                     required and tools it will call*, or drop the approval"
                ),
            });
        }
        if !agent.tools.iter().any(|held| held == tool) {
            return Err(AppError::Routine {
                field: "grants",
                reason: format!(
                    "`{}` does not hold `{tool}`, so approving it here would grant what the \
                     identity was refused",
                    agent.name
                ),
            });
        }
    }

    Ok(())
}

/// Why this routine cannot fire now, or `None`: the door re-checked on every
/// list and tick. A paused routine has no problem.
pub fn inspect(
    routine: &Routine,
    agent: Option<&Agent>,
    workspace: Option<&Path>,
    skill: Option<&Skill>,
    agent_runs_today: u32,
) -> Option<String> {
    let Some(agent) = agent else {
        return Some(
            "the identity this runs as is no longer on file, so nothing can run it. Point it at \
             another one, or delete it"
                .to_owned(),
        );
    };

    if workspace.is_none() {
        return Some(
            "this project's folder is not there right now, so a run would have no workspace"
                .to_owned(),
        );
    }

    if !agent.allows_skill(&routine.skill) {
        return Some(format!(
            "`{}` is no longer allowed to run `{}` — tick it again in Settings, or delete this \
             routine",
            agent.name, routine.skill
        ));
    }

    let Some(skill) = skill else {
        return Some(format!(
            "`{}` is not in the library or in this workspace any more",
            routine.skill
        ));
    };

    if let Some(problem) = &skill.problem {
        return Some(format!("`{}` will not parse: {problem}", routine.skill));
    }

    if let Some(missing) = skill
        .tools
        .iter()
        .find(|wanted| !agent.tools.iter().any(|held| held == *wanted))
    {
        return Some(format!(
            "`{}` calls `{missing}`, which `{}` no longer holds",
            routine.skill, agent.name
        ));
    }

    if routine.runs_today >= routine.runs_per_day {
        return Some(format!(
            "its {} runs for today are spent; it starts again tomorrow",
            routine.runs_per_day
        ));
    }

    // Last: the per-agent budget may be spent by other routines.
    if agent_runs_today >= agent.runs_per_day {
        return Some(format!(
            "`{}` has used its {} scheduled runs for today, across every routine that fires as \
             it",
            agent.name, agent.runs_per_day
        ));
    }

    None
}

// ---------------------------------------------------------------------------
// The clock
// ---------------------------------------------------------------------------

/// Whether this routine is due.
///
/// Pure: given `now` and, for [`Schedule::OnChange`], the watched directory's
/// newest stamp. A missed window fires once, never a backlog, and nothing
/// before `armed_at` counts.
pub fn due(routine: &Routine, now: DateTime<Utc>, watch: Option<&Watch>) -> bool {
    if routine.paused {
        return false;
    }

    let armed = parse(&routine.armed_at);
    let last = routine.last.as_ref().and_then(|last| parse(&last.at));

    match &routine.schedule {
        Schedule::Every { minutes } => {
            let since = last.or(armed);
            let Some(since) = since else {
                // No stamp reads back: fire, rather than never. A routine that
                // cannot say when it last ran is a routine whose worst failure
                // should be running once too often, not silence.
                return true;
            };
            now >= since + chrono::TimeDelta::minutes(i64::from(*minutes))
        }
        Schedule::DailyAt { hour, minute } => {
            let Some(slot) = last_slot(now, *hour, *minute) else {
                return false;
            };
            if armed.is_some_and(|armed| slot <= armed) {
                return false;
            }
            last.is_none_or(|last| last < slot)
        }
        Schedule::OnChange { .. } => {
            let Some(watch) = watch else {
                return false;
            };
            let Some(newest) = watch.newest.as_deref() else {
                return false;
            };
            // Nothing to act on: either this routine has never looked (see
            // [`learning`], which is the tick's job and not this one's) or
            // nothing has arrived since the change it last ran for.
            if watch.seen.is_empty() || newest <= watch.seen.as_str() {
                return false;
            }
            // Same floor as an interval, or a busy folder fires every tick.
            last.is_none_or(|last| {
                now >= last + chrono::TimeDelta::minutes(i64::from(EVERY_MIN_MINUTES))
            })
        }
    }
}

/// The newest change a routine has to *record* rather than act on.
///
/// `Some` only for a routine that has never recorded a watermark (usually
/// [`AppState::arm_watch`](crate::AppState::arm_watch) did at save). The only
/// case where the watermark moves without a run, so changes during a cooldown
/// are not swallowed.
pub fn learning(watch: &Watch) -> Option<&str> {
    if watch.seen.is_empty() {
        watch.newest.as_deref()
    } else {
        None
    }
}

/// The most recent local occurrence of `hour:minute` at or before `now`.
///
/// `None` for a local time a daylight-saving jump skips: that day it does not
/// fire.
fn last_slot(now: DateTime<Utc>, hour: u32, minute: u32) -> Option<DateTime<Utc>> {
    let local = now.with_timezone(&Local);

    for back in 0..2 {
        let day = local.date_naive() - chrono::TimeDelta::days(back);
        let naive = day.and_hms_opt(hour, minute, 0)?;
        // `earliest` rather than `single`: the hour a fall-back repeats exists
        // twice, and the first of the two is when a person expects it.
        if let Some(slot) = Local.from_local_datetime(&naive).earliest() {
            let slot = slot.with_timezone(&Utc);
            if slot <= now {
                return Some(slot);
            }
        }
    }

    None
}

/// The newest modification time under `dir`, as a fixed-width UTC stamp.
///
/// Bounded by [`WATCH_DEPTH`] and [`WATCH_ENTRIES`]; unreadable entries are
/// skipped.
pub fn newest_change(dir: &Path) -> Option<String> {
    let mut newest: Option<DateTime<Utc>> = None;
    let mut budget = WATCH_ENTRIES;
    let mut stack = vec![(dir.to_path_buf(), 0usize)];

    let mut consider = |modified: std::io::Result<std::time::SystemTime>| {
        if let Ok(modified) = modified {
            let at: DateTime<Utc> = modified.into();
            if newest.is_none_or(|held| at > held) {
                newest = Some(at);
            }
        }
    };

    while let Some((at, depth)) = stack.pop() {
        // Directory stamps too: a moved-in or copied file keeps its old mtime,
        // and a deletion leaves no file to ask.
        consider(std::fs::metadata(&at).and_then(|meta| meta.modified()));

        let Ok(entries) = std::fs::read_dir(&at) else {
            continue;
        };
        for entry in entries.flatten() {
            if budget == 0 {
                break;
            }
            budget -= 1;

            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                if depth + 1 < WATCH_DEPTH {
                    stack.push((entry.path(), depth + 1));
                }
                continue;
            }
            // A file rewritten in place, which the directory's stamp would not
            // notice.
            consider(meta.modified());
        }
    }

    newest.map(|at| at.to_rfc3339_opts(SecondsFormat::Millis, true))
}

/// Reads one of the store's timestamps; `None` (treated as no stamp) if it
/// will not parse.
fn parse(stamp: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(stamp)
        .ok()
        .map(|at| at.with_timezone(&Utc))
}

// ---------------------------------------------------------------------------
// What the run is told
// ---------------------------------------------------------------------------

/// The message a scheduled run opens with, built here since routines have no
/// prompt (PLAN 7.6): the runbook, that nobody is watching, and that asks are
/// refused.
pub fn opening(routine: &Routine, skill: &Skill) -> String {
    let mut out = format!(
        "This is a scheduled run. The routine `{}` fired it — {} — and it runs as this identity \
         with nobody in front of it.\n\nRun `skill:{}` now: call `skill_run` with that name, \
         follow the runbook it gives you, and finish with `skill_return`.\n\n",
        routine.name,
        routine.schedule.label(),
        skill.name,
    );

    out.push_str(
        "Three things are different from a session someone is typing into.\n\n\
         No dialog can be answered while this runs, so a call that would ask is parked instead: \
         it does not run, and the question is kept for a person to answer afterwards. That is not \
         a fault to work around. Keep what you have already done, do what you can without the \
         call, and finish with `skill_return` `blocked` naming what is parked — if it is allowed, \
         this run is picked up again with the answer.\n\n",
    );

    if routine.grants.is_empty() {
        out.push_str(
            "This routine was signed for nothing beyond reading, so a write or a command will be \
             parked rather than done.\n\n",
        );
    } else {
        out.push_str("This routine was signed for exactly this, and nothing else:\n");
        for grant in &routine.grants {
            out.push_str(&format!("- {}\n", grant.scope_label()));
        }
        out.push('\n');
    }

    out.push_str(
        "And nobody reads a reply. What lasts is what the runbook has you write into the \
         workspace, and the return itself — so put the answer in the file, and keep the return to \
         the status, the artefacts and anything a person has to decide.",
    );
    out
}

/// How a scheduled run's opening message begins; the scripted provider and
/// tests match on it.
pub const OPENING_MARKER: &str = "This is a scheduled run.";

/// A routine's name (not the skill's), cut to a session title.
pub fn title(routine: &Routine) -> String {
    let name = routine.name.trim();
    if name.chars().count() <= TITLE_MAX_CHARS {
        return name.to_owned();
    }
    format!(
        "{}…",
        name.chars()
            .take(TITLE_MAX_CHARS - 1)
            .collect::<String>()
            .trim_end()
    )
}

/// Most characters of a routine's name that become a session title.
const TITLE_MAX_CHARS: usize = 48;

#[cfg(test)]
mod tests {
    use chrono::Timelike as _;

    use super::*;
    use crate::store::routines::{LastRun, RunOutcome};

    fn stamp(at: DateTime<Utc>) -> String {
        at.to_rfc3339_opts(SecondsFormat::Millis, true)
    }

    fn routine(schedule: Schedule, armed: DateTime<Utc>) -> Routine {
        Routine {
            id: "r1".to_owned(),
            name: "Morning watch".to_owned(),
            project_id: "p1".to_owned(),
            agent_id: "a1".to_owned(),
            skill: "watch.digest".to_owned(),
            schedule,
            grants: Vec::new(),
            runs_per_day: 24,
            runs_today: 0,
            paused: false,
            paused_reason: String::new(),
            armed_at: stamp(armed),
            last: None,
            problem: None,
            created_at: stamp(armed),
            updated_at: stamp(armed),
        }
    }

    fn ran_at(routine: &mut Routine, at: DateTime<Utc>) {
        routine.last = Some(LastRun {
            at: stamp(at),
            session_id: "s1".to_owned(),
            outcome: RunOutcome::Done,
            detail: String::new(),
        });
    }

    /// `WorldAmend` can never be signed on a routine (PLAN 7.2), even in a
    /// hand-edited file.
    #[test]
    fn a_routine_cannot_be_signed_for_amending_the_world() {
        let mut agent = Agent::builtin();
        agent.id = "a1".to_owned();
        agent.skills = vec!["watch.digest".to_owned()];

        let skill = Skill {
            name: "watch.digest".to_owned(),
            scope: crate::skills::SkillScope::Library,
            version: "1".to_owned(),
            summary: "looks".to_owned(),
            tools: vec![crate::policy::tool::FS_WRITE.to_owned()],
            path: "watch.digest/SKILL.md".to_owned(),
            shadows: false,
            problem: None,
        };
        let draft = |grants: Vec<Grant>| RoutineDraft {
            name: "Morning watch".to_owned(),
            project_id: "p1".to_owned(),
            agent_id: "a1".to_owned(),
            skill: "watch.digest".to_owned(),
            schedule: Schedule::Every { minutes: 60 },
            grants,
            runs_per_day: 4,
        };

        // The ordinary write grant is fine: the runbook declares `fs_write` and
        // the identity holds it.
        check(&draft(vec![Grant::FsWrite]), &agent, Some(&skill), true).expect("signable");

        let refused = check(&draft(vec![Grant::WorldAmend]), &agent, Some(&skill), true)
            .expect_err("the world is not");
        assert!(refused.to_string().contains("human decision"), "{refused}");
    }

    #[test]
    fn an_interval_counts_from_the_last_run_not_from_a_grid() {
        let armed = Utc::now() - chrono::TimeDelta::hours(4);
        let mut routine = routine(Schedule::Every { minutes: 60 }, armed);

        // Never run: counted from the arming, which was four hours ago.
        assert!(due(&routine, Utc::now(), None));

        ran_at(&mut routine, Utc::now() - chrono::TimeDelta::minutes(59));
        assert!(!due(&routine, Utc::now(), None));

        ran_at(&mut routine, Utc::now() - chrono::TimeDelta::minutes(61));
        assert!(due(&routine, Utc::now(), None));
    }

    #[test]
    fn a_paused_routine_is_never_due() {
        let mut routine = routine(
            Schedule::Every { minutes: 5 },
            Utc::now() - chrono::TimeDelta::hours(9),
        );
        routine.paused = true;

        assert!(!due(&routine, Utc::now(), None));
    }

    /// The window a routine was armed after does not count, and the one after
    /// that fires exactly once however long the machine was asleep.
    #[test]
    fn a_daily_routine_fires_once_for_the_window_it_missed() {
        let now = Utc::now();
        let local = now.with_timezone(&Local);
        let (hour, minute) = (local.hour(), local.minute());

        // Armed at or after today's slot — the slot is this very minute — so
        // the routine is not immediately late for a window it was not up for.
        let mut routine = routine(Schedule::DailyAt { hour, minute }, now);
        assert!(!due(&routine, now, None));

        // Armed two days ago: today's slot has passed and nothing has run.
        routine.armed_at = stamp(now - chrono::TimeDelta::days(2));
        assert!(due(&routine, now, None));

        // It ran at that slot. Not due again until tomorrow's.
        ran_at(&mut routine, now);
        assert!(!due(&routine, now, None));

        // Asleep for a week: one run owed, not seven — which is what "is it
        // time yet" against the last run means.
        ran_at(&mut routine, now - chrono::TimeDelta::days(7));
        assert!(due(&routine, now, None));
    }

    #[test]
    fn a_trigger_learns_the_folder_before_it_fires() {
        let now = Utc::now();
        let routine = routine(
            Schedule::OnChange {
                dir: "briefs".to_owned(),
            },
            now - chrono::TimeDelta::hours(1),
        );

        // First look: the watermark is empty, so this is learning, not firing.
        assert!(!due(
            &routine,
            now,
            Some(&Watch {
                newest: Some(stamp(now)),
                seen: String::new(),
            })
        ));

        // Nothing newer than what it saw.
        assert!(!due(
            &routine,
            now,
            Some(&Watch {
                newest: Some(stamp(now - chrono::TimeDelta::minutes(5))),
                seen: stamp(now),
            })
        ));

        // Something newer.
        assert!(due(
            &routine,
            now,
            Some(&Watch {
                newest: Some(stamp(now)),
                seen: stamp(now - chrono::TimeDelta::minutes(5)),
            })
        ));
    }

    #[test]
    fn a_trigger_does_not_fire_faster_than_the_interval_floor() {
        let now = Utc::now();
        let mut routine = routine(
            Schedule::OnChange {
                dir: "briefs".to_owned(),
            },
            now - chrono::TimeDelta::hours(1),
        );
        ran_at(&mut routine, now - chrono::TimeDelta::minutes(1));

        let watch = Watch {
            newest: Some(stamp(now)),
            seen: stamp(now - chrono::TimeDelta::minutes(30)),
        };
        assert!(!due(&routine, now, Some(&watch)));

        ran_at(
            &mut routine,
            now - chrono::TimeDelta::minutes(i64::from(EVERY_MIN_MINUTES) + 1),
        );
        assert!(due(&routine, now, Some(&watch)));
    }

    /// A change during the cooldown is still pending when it expires.
    #[test]
    fn a_change_during_the_cooldown_is_deferred_and_never_swallowed() {
        let now = Utc::now();
        let mut routine = routine(
            Schedule::OnChange {
                dir: "briefs".to_owned(),
            },
            now - chrono::TimeDelta::hours(1),
        );
        ran_at(&mut routine, now - chrono::TimeDelta::minutes(1));

        // A file lands a minute after a run: newer than the watermark, and
        // inside the floor.
        let watch = Watch {
            newest: Some(stamp(now)),
            seen: stamp(now - chrono::TimeDelta::minutes(30)),
        };
        assert!(!due(&routine, now, Some(&watch)));
        assert_eq!(
            learning(&watch),
            None,
            "a routine that has looked before never re-learns, so nothing moves its watermark \
             but a run"
        );

        // The floor expires with the same watch, and it fires.
        let later = now + chrono::TimeDelta::minutes(i64::from(EVERY_MIN_MINUTES));
        assert!(due(&routine, later, Some(&watch)));
    }

    #[test]
    fn only_a_routine_that_has_never_looked_is_learning() {
        let now = Utc::now();
        assert_eq!(
            learning(&Watch {
                newest: Some(stamp(now)),
                seen: String::new(),
            }),
            Some(stamp(now).as_str())
        );
        assert_eq!(
            learning(&Watch {
                newest: None,
                seen: String::new(),
            }),
            None,
            "an empty or missing folder teaches nothing"
        );
    }

    #[test]
    fn the_newest_change_is_the_newest_stamp_under_the_directory() {
        let dir = tempfile::TempDir::new().expect("temp dir");

        // Never empty: the directory itself has a stamp, which is what makes an
        // empty folder something a routine can watch for a first arrival in.
        let empty = newest_change(dir.path()).expect("a directory has a stamp");
        assert!(empty.ends_with('Z'), "{empty}");

        std::fs::create_dir_all(dir.path().join("nested")).expect("nested");
        std::fs::write(dir.path().join("nested/one.md"), "one").expect("write");
        let with_file = newest_change(dir.path()).expect("a stamp");
        assert!(with_file >= empty, "{with_file} vs {empty}");
    }

    /// A moved-in file (old mtime) and a deletion both register through the
    /// directory's own stamp.
    #[test]
    fn removing_a_file_is_a_change_even_though_no_file_is_newer() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let file = dir.path().join("brief.md");
        std::fs::write(&file, "an item").expect("write");

        let before = newest_change(dir.path()).expect("a stamp");

        // A directory's stamp has one-second granularity on some filesystems,
        // so the change has to be given a moment to be distinguishable at all.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::remove_file(&file).expect("remove");

        let after = newest_change(dir.path()).expect("the directory is still there");
        assert!(
            after > before,
            "a removal has to read as a change: {after} is not after {before}"
        );
    }
}
