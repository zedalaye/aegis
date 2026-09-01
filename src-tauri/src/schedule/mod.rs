//! Routines: a clock on a skill (PLAN 7.3, Phase 16).
//!
//! This module is the *policy* of the scheduler — which routine may exist, when
//! one is due, and what its run is told. [`runner`] is the machinery that then
//! opens a session and drives it, and the split is the one Phase 15 drew
//! between `handoff::bus` and `handoff::runner`, for the same reason: the rules
//! here are arithmetic and refusals, and every one of them is exercisable
//! without an application, a clock or a model.
//!
//! ## The door
//!
//! A routine names a **live skill, already granted, already run under watch at
//! least once** (PLAN 7.13, *Phase 16's door*). Those three are checked in
//! [`check`], and the third is the one worth explaining, because it is the only
//! check in the runtime whose evidence is the audit log: a routine may only
//! name a runbook this identity has already carried to a `skill_return`. The
//! audit line for that return exists precisely because PLAN 7.6 asks for it —
//! "a run without `skill` on the line cannot be budgeted or replayed" — and
//! this is what it buys. It is not a formality. "Promote a procedure to a
//! skill, run it under watch, *then* put it on a clock" is the whole discipline
//! of § 7.6, and without a check the middle step is the one everybody skips.
//!
//! There is no fourth door offering to waive the other three. A skill that will
//! not parse, one the identity was never granted, one nobody has run: each is a
//! refusal with the fix in it, at the moment somebody saves the routine, rather
//! than a silent failure at four in the morning.
//!
//! ## Nobody is watching
//!
//! That is the fact the whole phase turns on, and it is deliberately *not*
//! conditional on whether the window happens to be open: a run that behaved
//! differently depending on where the window was would be one nobody could
//! reproduce. So a routine's run is always unattended, `Run now` included —
//! what you see when you press it is exactly what the clock does at three in
//! the morning.
//!
//! Unattended means an approval dialog cannot be answered, so [`policy`] turns
//! every *ask* into a refusal ([`PolicyCtx::unattended`]) instead of parking a
//! turn on a prompt nobody will ever see. What a run may do beyond reading is
//! therefore exactly the list of grants a person signed on the routine — the
//! same [`Grant`](crate::policy::Grant) values the approval dialog creates,
//! seeded into the run's session and dropped when it ends. [`check`] refuses to
//! store one the runbook does not declare it will call, or one the identity
//! does not hold, so the signature is bounded twice over by things somebody
//! already decided.
//!
//! ## What a routine cannot be
//!
//! It cannot be a chat: there is no prompt field anywhere in this phase, and
//! the run's opening message is written here ([`opening`]) from the routine and
//! the runbook. It cannot be a proposal: only `SKILL.md` reaches the catalog
//! (PLAN 7.13). It cannot grant itself anything: the signature is checked
//! against the identity's allow-list, which only Settings can widen. And it
//! cannot run forever: a routine that ends twice running with no report pauses
//! itself and says why.

pub mod runner;

use std::path::Path;

use chrono::{DateTime, Local, SecondsFormat, TimeZone as _, Utc};

use crate::error::{AppError, AppResult};
use crate::skills::Skill;
use crate::store::routines::{Routine, RoutineDraft, Schedule, EVERY_MIN_MINUTES};
use crate::store::Agent;

/// How often the scheduler looks at its routines.
///
/// Half a minute: short enough that "daily at 07:00" fires at 07:00 rather than
/// at some time after it, long enough that the process is asleep essentially
/// always. Nothing in the phase depends on it being exact — every schedule is
/// expressed as "is it time yet", never as "wake me at", so a tick that is late
/// costs lateness and never a missed run.
pub const TICK: std::time::Duration = std::time::Duration::from_secs(30);

/// Most scheduled runs that may be in flight at once, across every routine.
///
/// A ceiling on the whole scheduler rather than on any one routine, because
/// what a person notices is the machine, not the routine: ten runbooks that all
/// fire at nine o'clock are ten sessions, ten model requests and ten
/// `shell_exec` children. The rest are not dropped — they are simply not due
/// yet on this tick, and the next one takes them.
pub const MAX_IN_FLIGHT: usize = 2;

/// How deep under a watched directory a change is looked for.
const WATCH_DEPTH: usize = 3;

/// How many entries a watched directory walk will visit before it stops.
///
/// A trigger is a cheap question asked often; a routine pointed at a folder of
/// fifty thousand files must not turn every tick into a filesystem sweep. The
/// walk stops and answers with the newest it saw, which is the honest answer to
/// "has anything changed" for a folder somebody is using as an inbox.
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
/// `skill` is the catalog entry for the name the draft carries, `None` when the
/// catalog has no such name; `witnessed` is whether the audit log holds a
/// `skill_return` this identity made for it. Both are measured by the caller,
/// which is what keeps this function a pure statement of the rules.
///
/// Every refusal names the fix, because all four of them are fixable and three
/// of them are fixable in a place the person is not currently looking.
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

    // Fail closed at the door as well as at the run (PLAN 7.6, *No extra
    // rights*). The runner would refuse this anyway, before the first step —
    // saying so now is the difference between a routine that never works and a
    // form that explains why.
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

/// Why this routine cannot fire as it stands, or `None` when it can.
///
/// The derived half of the door: [`check`] runs when somebody saves, and this
/// runs on every list and every tick, because everything it looks at can change
/// afterwards without the routine being touched. A skill can be un-granted in
/// Settings, a workspace folder can be unplugged, an identity can be deleted.
/// The routine is left alone in all three cases — it is a record of what
/// somebody wanted — and it says what is wrong instead of firing.
///
/// A paused routine has no problem: pausing is a decision, not a fault.
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

    // The other half of "budget per agent and per routine" (`COS.md`). It is
    // last because it is the one a person is least likely to have caused with
    // this routine: three well-behaved clocks on one identity can spend it
    // between them, and the row should say so rather than reading as broken.
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
/// Pure, and given everything it needs: `now` and — for a
/// [`Schedule::OnChange`] — what the watched directory looks like. A function
/// that read the clock itself would be one no test could put at 06:59 and then
/// at 07:01.
///
/// Two rules are shared by all three schedules and are worth stating once.
///
/// **A missed window fires once, never a backlog.** A machine that was asleep
/// for a week owes one run, not two hundred. That falls out of asking "is it
/// time yet" against the *last run* rather than counting slots.
///
/// **Nothing before `armed_at` counts.** A routine saved at three in the
/// afternoon and set to run daily at seven is not immediately eight hours late;
/// it starts counting when it was armed. Editing the schedule re-arms it, and
/// so does un-pausing.
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
            // The same floor an interval has. A folder somebody is actively
            // writing into would otherwise fire this on every tick, and a
            // trigger that runs every thirty seconds is the runaway
            // `EVERY_MIN_MINUTES` exists to prevent.
            last.is_none_or(|last| {
                now >= last + chrono::TimeDelta::minutes(i64::from(EVERY_MIN_MINUTES))
            })
        }
    }
}

/// The newest change a routine has to *record* rather than act on.
///
/// `Some` only for a routine that has never looked at its folder: there is no
/// "before" to compare against, and a routine pointed at a directory of old
/// files is watching for the next one, not announcing the last hundred.
///
/// Normally nothing takes this path — saving a routine records where "now" is
/// ([`AppState::arm_watch`](crate::AppState::arm_watch)), so the first tick
/// already has a watermark and a file dropped in a second after saving fires.
/// It is the fallback for when that look failed: an unplugged folder, a
/// permission error.
///
/// It is deliberately the *only* case in which the watermark moves without a
/// run. Advancing it on every tick would swallow any change that arrived while
/// the routine was inside its cooldown — marked as seen by a tick that did not
/// fire, with nothing newer ever to come.
pub fn learning(watch: &Watch) -> Option<&str> {
    if watch.seen.is_empty() {
        watch.newest.as_deref()
    } else {
        None
    }
}

/// The most recent local occurrence of `hour:minute` at or before `now`.
///
/// `None` only for the local times that do not exist — the hour a
/// daylight-saving jump skips. A routine set to 02:30 in a zone that goes from
/// 02:00 to 03:00 that night simply does not fire that day, which is the same
/// thing every alarm clock does and a better answer than firing twice.
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
/// Bounded in both directions ([`WATCH_DEPTH`], [`WATCH_ENTRIES`]) because this
/// runs on every tick. Unreadable entries are skipped rather than reported: a
/// permission error in one subdirectory is not a reason to stop watching the
/// folder, and the trigger's only claim is "something in here is newer than it
/// was".
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
        // The directory's own stamp, and not only its files'. An entry that
        // appears, disappears or is renamed touches the directory and may touch
        // no file at all: a file *moved* into the folder keeps the modification
        // time it had somewhere else — which on Windows is what copying one
        // does too — and a deletion has no file left to ask. Watching only the
        // files makes both of those invisible, which is not what "when this
        // folder changes" means to the person who wrote it.
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

/// Reads one of the store's timestamps.
///
/// `None` for anything that will not parse, which is a hand-edited document
/// rather than a state this process produces. Every caller treats that as "no
/// stamp" rather than as an error, because a routine is not worth refusing to
/// schedule over a mangled date.
fn parse(stamp: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(stamp)
        .ok()
        .map(|at| at.with_timezone(&Utc))
}

// ---------------------------------------------------------------------------
// What the run is told
// ---------------------------------------------------------------------------

/// The message a scheduled run opens with.
///
/// Written here rather than stored on the routine, and that is the phase's
/// central refusal: there is no prompt field, so there is nothing for a
/// still-fuzzy workflow to hide in (PLAN 7.6). What the model gets is the
/// runbook's name and the three facts it cannot read off the routine — that
/// nobody is watching, what that means for a call it wants approved, and that
/// the only thing anybody will ever read is what it writes down.
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
         No dialog can be answered while this runs, so any call that would ask is refused \
         outright rather than queued. That is not a fault to work around: if you need something \
         you were not approved for, return `blocked` and name it.\n\n",
    );

    if routine.grants.is_empty() {
        out.push_str(
            "This routine was signed for nothing beyond reading, so a write or a command will be \
             refused.\n\n",
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

/// How a scheduled run's opening message begins.
///
/// A constant because two other places match on it: the scripted provider, so
/// the whole phase can be walked through without a model, and the tests. A
/// marker in a message rather than a flag on the request, because that is all a
/// model ever gets — an unattended run has no channel of its own, and inventing
/// one would be a second way for a turn to be told something.
pub const OPENING_MARKER: &str = "This is a scheduled run.";

/// A routine's name, cut to a session title.
///
/// The routine's rather than the skill's: the sidebar row is answering "why is
/// this session here", and the runbook's name is already on every audit line
/// the run writes.
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

    /// The bug this rule exists to prevent: a change that arrives while the
    /// routine is inside its cooldown must still be pending when the cooldown
    /// expires. Only a run — or a first look with nothing to compare against —
    /// moves the watermark.
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
            "a routine that has looked before never re-learns, so nothing moves its watermark              but a run"
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

    /// The bug a real folder found: a file moved back in keeps the modification
    /// time it had elsewhere, and a deleted file has none at all. Both change
    /// the directory, and the directory is what the watch has to read — a walk
    /// over files alone reports "nothing happened" for either.
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
