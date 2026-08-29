//! Which sessions are doing something, and how to stop them.
//!
//! Two questions, one map, because they have the same answer. "Is this session
//! running?" is what the sidebar draws and what `session_send` refuses on; "how
//! do I cancel it?" is what `session_cancel` needs. Keeping them apart would
//! mean two structures that must agree about the same fact, and the moment
//! they disagree the UI shows a spinner nothing can stop.
//!
//! Nothing here is persisted, and that is the point. A [`SessionState`] is a
//! fact about this process: a session that was running when the machine lost
//! power is idle when it comes back, because there is no turn left to finish.
//! The session document stores the transcript; this holds what is happening to
//! it right now (see [`store::sessions`](crate::store::sessions)).
//!
//! Two records are kept per session rather than one. The `running` half is the
//! turn in flight; the `resting` half is what the session looks like when
//! there is none — `Idle`, or `Error` after a turn that failed, so the sidebar
//! can still show something went wrong after the task is gone. A send clears
//! the resting state, which is what makes an error badge disappear when the
//! user tries again.

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
}

/// A turn currently executing.
#[derive(Debug)]
struct ActiveTurn {
    id: String,
    cancel: CancellationToken,
}

impl TurnRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Locks the map, recovering from a poisoned mutex.
    ///
    /// The guarded value is a plain map replaced entry by entry, so it cannot
    /// be torn; propagating a panic through every later command would be
    /// strictly worse than carrying on with it.
    fn sessions(&self) -> MutexGuard<'_, HashMap<String, Live>> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Registers a turn, and hands back the token that cancels it.
    ///
    /// Fails with `E_TURN_BUSY` when the session already has one. That refusal
    /// is the whole reason this is a registry rather than a counter: two turns
    /// in one session would interleave their messages in the transcript, and
    /// the second would build its request from a conversation the first was
    /// halfway through writing.
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
        });
        // A new turn is the user trying again, so a stale error badge goes.
        live.resting = SessionState::Idle;

        tracing::debug!(session_id, turn_id, "turn registered");
        Ok(cancel)
    }

    /// Retires a turn and records what the session rests at.
    ///
    /// `turn_id` is checked rather than trusted: a task finishing after its
    /// session started a *newer* turn must not clear the new one. That is
    /// reachable — a cancelled turn's task can outlive the cancel by however
    /// long its last await takes.
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
    /// Fails when the turn named is not the one running, rather than
    /// succeeding silently: the caller is holding a handle from a turn that
    /// has already ended, and saying so is what makes the UI refetch instead
    /// of leaving a stop button that does nothing.
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
            Some(_) => SessionState::Running,
            None => live.resting,
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
}
