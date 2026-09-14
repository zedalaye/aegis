//! Which sessions are doing something, and how to stop them.
//!
//! One map answers both "is it running?" and "how do I cancel it?". Nothing is
//! persisted: after a crash every session is idle
//! ([`store::sessions`](crate::store::sessions)).
//!
//! Per session: `running` (the turn in flight) and `resting` (`Idle`, or
//! `Error` after a failed turn, cleared by the next send).

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use tokio_util::sync::CancellationToken;

use crate::error::{AppError, AppResult};
use crate::store::SessionState;

/// The live state of every session that has one.
#[derive(Debug, Default)]
pub struct TurnRegistry {
    sessions: Mutex<HashMap<String, Live>>,
}

/// What is happening to one session.
#[derive(Debug, Default)]
struct Live {
    /// The turn in flight.
    running: Option<ActiveTurn>,
    /// What the session shows when no turn is running.
    resting: SessionState,
    /// The runbook this session is part-way through, if any.
    run: Option<OpenRun>,
}

/// Most turns one skill run may span before it is dropped.
///
/// Past this, a run without `skill_return` is dropped loudly rather than
/// tagging later calls. Real runs span several turns (`IDEAS.md` § 10; see
/// [`MAX_TOOL_ROUNDS_IN_SKILL`](super::turn::MAX_TOOL_ROUNDS_IN_SKILL)).
pub const MAX_RUN_TURNS: u32 = 4;

/// A runbook a session is part-way through (PLAN 7.6).
///
/// Session state, not turn-local: the round cap can split a run across turns,
/// and its audit lines must keep the skill name (PLAN 7.6). Not persisted.
#[derive(Debug, Clone)]
struct OpenRun {
    /// The runbook being followed.
    skill: String,
    /// How many turns it has spanned, including the one that opened it.
    turns: u32,
}

/// A turn currently executing.
#[derive(Debug)]
struct ActiveTurn {
    id: String,
    cancel: CancellationToken,
    /// Whether the turn is parked on an approval rather than working.
    ///
    /// So the sidebar can show "waiting for you" rather than a spinner.
    waiting: bool,
}

impl TurnRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Locks the map, recovering from poison: it cannot be left torn.
    fn sessions(&self) -> MutexGuard<'_, HashMap<String, Live>> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Registers a turn, and hands back the token that cancels it.
    ///
    /// Fails with `E_TURN_BUSY` when the session already has one, so two turns
    /// never interleave a transcript.
    pub fn begin(&self, session_id: &str, turn_id: &str) -> AppResult<CancellationToken> {
        let mut sessions = self.sessions();
        let live = sessions.entry(session_id.to_owned()).or_default();

        if let Some(active) = &live.running {
            return Err(AppError::TurnBusy {
                turn_id: active.id.clone(),
            });
        }

        let cancel = CancellationToken::new();
        live.running = Some(ActiveTurn {
            id: turn_id.to_owned(),
            cancel: cancel.clone(),
            waiting: false,
        });
        // A new turn is the user trying again, so a stale error badge goes.
        live.resting = SessionState::Idle;

        tracing::debug!(session_id, turn_id, "turn registered");
        Ok(cancel)
    }

    /// Retires a turn and records what the session rests at.
    ///
    /// Checks `turn_id`, so a cancelled task finishing late cannot clear a
    /// newer turn.
    pub fn finish(&self, session_id: &str, turn_id: &str, resting: SessionState) {
        let mut sessions = self.sessions();
        let Some(live) = sessions.get_mut(session_id) else {
            return;
        };

        match &live.running {
            Some(active) if active.id == turn_id => {
                live.running = None;
                live.resting = resting;
                tracing::debug!(session_id, turn_id, ?resting, "turn retired");
            }
            Some(active) => tracing::debug!(
                session_id,
                finished = turn_id,
                current = %active.id,
                "a superseded turn finished; the current one keeps the session"
            ),
            None => {}
        }
    }

    /// Cancels a session's turn.
    ///
    /// Fails when the named turn is not the one running, so the UI refetches.
    pub fn cancel(&self, session_id: &str, turn_id: &str) -> AppResult<()> {
        let sessions = self.sessions();

        match sessions
            .get(session_id)
            .and_then(|live| live.running.as_ref())
        {
            Some(active) if active.id == turn_id => {
                active.cancel.cancel();
                tracing::info!(session_id, turn_id, "turn cancelled by the user");
                Ok(())
            }
            _ => {
                tracing::debug!(session_id, turn_id, "nothing to cancel");
                Err(AppError::Internal {
                    what: "that turn has already finished",
                })
            }
        }
    }

    /// Cancels whatever a session is running, if anything. Used on delete and
    /// on shutdown, where there is no handle to check against.
    pub fn cancel_any(&self, session_id: &str) {
        let sessions = self.sessions();
        if let Some(active) = sessions
            .get(session_id)
            .and_then(|live| live.running.as_ref())
        {
            active.cancel.cancel();
            tracing::debug!(session_id, turn_id = %active.id, "turn cancelled");
        }
    }

    /// Cancels every running turn. Called on quit so tasks stop rather than
    /// being torn down mid-write.
    pub fn cancel_all(&self) {
        let sessions = self.sessions();
        for live in sessions.values() {
            if let Some(active) = &live.running {
                active.cancel.cancel();
            }
        }
    }

    /// What a session is doing right now.
    ///
    /// A session nothing has touched is [`SessionState::Idle`], which is what
    /// every session is after a restart.
    pub fn state_of(&self, session_id: &str) -> SessionState {
        let sessions = self.sessions();
        let Some(live) = sessions.get(session_id) else {
            return SessionState::Idle;
        };

        match &live.running {
            Some(active) if active.waiting => SessionState::AwaitingApproval,
            Some(_) => SessionState::Running,
            None => live.resting,
        }
    }

    /// Marks a turn as blocked on an approval, or working again.
    ///
    /// Checked against `turn_id`, like [`TurnRegistry::finish`].
    pub fn set_waiting(&self, session_id: &str, turn_id: &str, waiting: bool) {
        let mut sessions = self.sessions();
        let Some(active) = sessions
            .get_mut(session_id)
            .and_then(|live| live.running.as_mut())
        else {
            return;
        };
        if active.id == turn_id {
            active.waiting = waiting;
        }
    }

    /// The turn a session is running, if any.
    pub fn active_turn(&self, session_id: &str) -> Option<String> {
        let sessions = self.sessions();
        sessions
            .get(session_id)
            .and_then(|live| live.running.as_ref())
            .map(|active| active.id.clone())
    }

    /// The runbook this session is part-way through, if any.
    ///
    /// Read at the top of a turn, so a procedure the round cap interrupted
    /// carries on under its own name instead of continuing anonymously.
    pub fn open_run(&self, session_id: &str) -> Option<String> {
        self.sessions()
            .get(session_id)
            .and_then(|live| live.run.as_ref())
            .map(|run| run.skill.clone())
    }

    /// Records what the turn that just ended was still following.
    ///
    /// `None` closes the run, which is what a `skill_return` and a cancelled
    /// turn both amount to. `Some` keeps it for the next turn and returns how
    /// many it has now spanned — or `None` when it has spent
    /// [`MAX_RUN_TURNS`] and was dropped, which the caller says out loud.
    pub fn carry_run(&self, session_id: &str, skill: Option<&str>) -> Option<u32> {
        let mut sessions = self.sessions();
        let live = sessions.entry(session_id.to_owned()).or_default();

        let Some(skill) = skill else {
            live.run = None;
            return None;
        };

        let turns = match &live.run {
            // The same run, one turn older.
            Some(open) if open.skill == skill => open.turns.saturating_add(1),
            // A different runbook, or the first turn of this one. Either way
            // the count starts here: what the previous name spent is not this
            // run's budget.
            _ => 1,
        };

        if turns > MAX_RUN_TURNS {
            live.run = None;
            return None;
        }

        live.run = Some(OpenRun {
            skill: skill.to_owned(),
            turns,
        });
        Some(turns)
    }

    /// Drops everything known about a session. Called when one is deleted.
    pub fn forget(&self, session_id: &str) {
        self.cancel_any(session_id);
        self.sessions().remove(session_id);
    }

    /// A closure over the current states, for
    /// [`SessionStore::list`](crate::store::SessionStore::list).
    pub fn lookup(&self) -> impl Fn(&str) -> SessionState + '_ {
        move |id| self.state_of(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The correction of `IDEAS.md` § 10: a procedure the round cap
    /// interrupted resumes under its own name.
    #[test]
    fn a_run_carries_to_the_next_turn_until_it_returns() {
        let registry = TurnRegistry::new();
        assert_eq!(registry.open_run("s1"), None, "nothing is running");

        assert_eq!(registry.carry_run("s1", Some("review.diff")), Some(1));
        assert_eq!(registry.open_run("s1").as_deref(), Some("review.diff"));

        assert_eq!(registry.carry_run("s1", Some("review.diff")), Some(2));
        assert_eq!(registry.open_run("s1").as_deref(), Some("review.diff"));

        // What a `skill_return` and a cancelled turn both amount to.
        registry.carry_run("s1", None);
        assert_eq!(registry.open_run("s1"), None);
    }

    /// The bound on the risk that made a run turn-scoped: a name nobody closes
    /// is dropped rather than carried through a conversation that moved on.
    #[test]
    fn a_run_nobody_returns_is_dropped_once_it_has_spent_its_turns() {
        let registry = TurnRegistry::new();

        for turn in 1..=MAX_RUN_TURNS {
            assert_eq!(registry.carry_run("s1", Some("cos.loop")), Some(turn));
        }
        assert_eq!(
            registry.carry_run("s1", Some("cos.loop")),
            None,
            "the turn past the ceiling drops it"
        );
        assert_eq!(registry.open_run("s1"), None);
    }

    /// A second runbook is a second run, and does not inherit what the first
    /// one had already spent.
    #[test]
    fn a_different_runbook_starts_its_own_count() {
        let registry = TurnRegistry::new();

        assert_eq!(registry.carry_run("s1", Some("world.check")), Some(1));
        assert_eq!(registry.carry_run("s1", Some("world.check")), Some(2));
        assert_eq!(registry.carry_run("s1", Some("review.diff")), Some(1));
        assert_eq!(registry.open_run("s1").as_deref(), Some("review.diff"));
    }

    /// A run belongs to one session, like everything else in this map.
    #[test]
    fn one_sessions_run_is_not_anothers() {
        let registry = TurnRegistry::new();

        registry.carry_run("s1", Some("review.diff"));
        assert_eq!(registry.open_run("s2"), None);

        registry.forget("s1");
        assert_eq!(
            registry.open_run("s1"),
            None,
            "a deleted session keeps none"
        );
    }

    #[test]
    fn an_untouched_session_is_idle() {
        let registry = TurnRegistry::new();

        assert_eq!(registry.state_of("s1"), SessionState::Idle);
        assert_eq!(registry.active_turn("s1"), None);
    }

    #[test]
    fn a_running_session_refuses_a_second_turn() {
        let registry = TurnRegistry::new();
        registry.begin("s1", "t1").expect("the first turn starts");

        let err = registry
            .begin("s1", "t2")
            .expect_err("the second is refused");
        assert_eq!(err.code(), crate::error::ErrorCode::TurnBusy);
        assert!(matches!(err, AppError::TurnBusy { turn_id } if turn_id == "t1"));

        assert_eq!(registry.state_of("s1"), SessionState::Running);
        assert_eq!(registry.active_turn("s1").as_deref(), Some("t1"));

        // A different session is unaffected.
        assert!(registry.begin("s2", "t3").is_ok());
    }

    #[test]
    fn finishing_leaves_the_session_at_its_resting_state() {
        let registry = TurnRegistry::new();

        registry.begin("s1", "t1").expect("begin");
        registry.finish("s1", "t1", SessionState::Error);
        assert_eq!(registry.state_of("s1"), SessionState::Error);

        // Sending again clears the error, which is what makes the badge go.
        registry.begin("s1", "t2").expect("begin again");
        assert_eq!(registry.state_of("s1"), SessionState::Running);
        registry.finish("s1", "t2", SessionState::Idle);
        assert_eq!(registry.state_of("s1"), SessionState::Idle);
    }

    /// A cancelled turn's task can outlive the cancel by as long as its last
    /// await takes. If it retired the session blindly it would clear a newer
    /// turn the user had already started.
    #[test]
    fn a_superseded_turn_cannot_retire_the_current_one() {
        let registry = TurnRegistry::new();

        registry.begin("s1", "t1").expect("begin");
        registry.finish("s1", "t1", SessionState::Idle);
        registry.begin("s1", "t2").expect("begin again");

        registry.finish("s1", "t1", SessionState::Error);

        assert_eq!(registry.state_of("s1"), SessionState::Running);
        assert_eq!(registry.active_turn("s1").as_deref(), Some("t2"));
    }

    #[tokio::test]
    async fn cancelling_trips_the_token_the_turn_holds() {
        let registry = TurnRegistry::new();
        let cancel = registry.begin("s1", "t1").expect("begin");

        assert!(!cancel.is_cancelled());
        registry.cancel("s1", "t1").expect("cancel");
        assert!(cancel.is_cancelled());

        // Awaiting it resolves immediately, which is what the turn loop does.
        cancel.cancelled().await;
    }

    #[test]
    fn cancelling_a_turn_that_has_ended_says_so() {
        let registry = TurnRegistry::new();
        registry.begin("s1", "t1").expect("begin");

        assert!(
            registry.cancel("s1", "t2").is_err(),
            "a stale handle is reported, not silently accepted"
        );
        assert!(registry.cancel("s2", "t1").is_err(), "no such session");

        registry.finish("s1", "t1", SessionState::Idle);
        assert!(registry.cancel("s1", "t1").is_err(), "already finished");
    }

    #[test]
    fn forgetting_a_session_cancels_it_first() {
        let registry = TurnRegistry::new();
        let cancel = registry.begin("s1", "t1").expect("begin");

        registry.forget("s1");

        assert!(cancel.is_cancelled());
        assert_eq!(registry.state_of("s1"), SessionState::Idle);
    }

    #[test]
    fn quitting_cancels_everything_in_flight() {
        let registry = TurnRegistry::new();
        let first = registry.begin("s1", "t1").expect("begin");
        let second = registry.begin("s2", "t2").expect("begin");

        registry.cancel_all();

        assert!(first.is_cancelled());
        assert!(second.is_cancelled());
    }

    #[test]
    fn the_lookup_reports_what_the_registry_holds() {
        let registry = TurnRegistry::new();
        registry.begin("s1", "t1").expect("begin");
        registry.finish("s2", "t9", SessionState::Error);

        let lookup = registry.lookup();
        assert_eq!(lookup("s1"), SessionState::Running);
        assert_eq!(lookup("s3"), SessionState::Idle);
    }

    /// A session waiting on a click is not a session that is working, and the
    /// sidebar has to be able to tell them apart (PLAN 2.1, `SessionState`).
    #[test]
    fn a_turn_parked_on_an_approval_says_so() {
        let registry = TurnRegistry::new();
        registry.begin("s1", "t1").expect("free");
        assert_eq!(registry.state_of("s1"), SessionState::Running);

        registry.set_waiting("s1", "t1", true);
        assert_eq!(registry.state_of("s1"), SessionState::AwaitingApproval);

        registry.set_waiting("s1", "t1", false);
        assert_eq!(registry.state_of("s1"), SessionState::Running);
    }

    #[test]
    fn a_superseded_turn_cannot_park_the_session_it_no_longer_owns() {
        let registry = TurnRegistry::new();
        registry.begin("s1", "t1").expect("free");
        registry.finish("s1", "t1", SessionState::Idle);
        registry.begin("s1", "t2").expect("free again");

        registry.set_waiting("s1", "t1", true);

        assert_eq!(
            registry.state_of("s1"),
            SessionState::Running,
            "the finished turn must not repaint its successor"
        );
    }
}
