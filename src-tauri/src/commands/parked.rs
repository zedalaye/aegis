//! Parked asks: what is waiting for a person, and answering it (PLAN 7.22).
//!
//! The board reads [`parked_list`] and answers with [`parked_answer`]. The run
//! is claimed, then the answer recorded, then the run picked up — in that
//! order, so a question is never taken off the board by an answer that could
//! not start anything.
//!
//! Nothing here approves by itself: the three answers are the three the dialog
//! has always offered, and a notification is not one of them.

use tauri::{AppHandle, Runtime, State};

use crate::agent::event::{Event, EventSink, ParkedResolved};
use crate::approval::Decision;
use crate::error::AppResult;
use crate::park;
use crate::schedule::runner;
use crate::state::AppState;
use crate::store::{ParkedAsk, SessionState};

use super::session::WindowSink;

/// What is waiting for a person, oldest first.
///
/// `project_id` omitted is every project's. The list is authoritative: a park
/// missing from it cannot be answered, whatever the board still shows.
#[tauri::command(rename_all = "snake_case")]
pub fn parked_list(
    state: State<'_, AppState>,
    project_id: Option<String>,
) -> AppResult<Vec<ParkedAsk>> {
    Ok(state.parked_list(project_id.as_deref()))
}

/// Answers one parked ask, and picks its run up.
///
/// `allow_once` mints a one-shot on that exact call, `allow_session` signs a
/// standing approval onto the routine (through Phase 16's door, PLAN 7.13),
/// and `deny` records the refusal. All three resume the run, so it can finish
/// or return `blocked` rather than being left half-done.
///
/// Errors: `E_APPROVAL_STALE` (answered already, or expired — refetch),
/// `E_GRANT_NOT_ALLOWED` (no standing approval is on offer for this row),
/// `E_INVALID_SETTING` (the routine's door refuses the grant, or its run is
/// already going) and `E_TURN_BUSY` (the session is working on something).
#[tauri::command(rename_all = "snake_case")]
pub fn parked_answer<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    parked_id: String,
    decision: Decision,
) -> AppResult<()> {
    // Read before it is answered, so the run can be claimed first.
    let waiting = state.parked().get(&parked_id)?;

    let resume = Resume::claim(&state, &waiting)?;
    let answered = match state.answer_parked(&parked_id, decision) {
        Ok(answered) => answered,
        Err(err) => {
            resume.give_up(&state, &waiting);
            return Err(err);
        }
    };

    let sink = WindowSink::new(app.clone());
    if decision == Decision::Deny {
        sink.emit(Event::AuditAppended(Box::new(
            state.audit_parked_refusal(&answered),
        )));
    }
    sink.emit(Event::ParkedResolved(ParkedResolved {
        id: answered.id.clone(),
        session_id: answered.session_id.clone(),
        answer: park::answer_word(decision).to_owned(),
    }));

    resume.go(&app, &answered, decision);
    Ok(())
}

/// The claim on whatever will run the answer: a scheduler slot for a routine's
/// run, a registered turn for a session someone opened.
///
/// Taken before the answer is recorded and given back if recording fails, so
/// the board and the runtime cannot disagree about whether an answer happened.
enum Resume {
    /// A routine's run, picked up by the scheduler.
    Routine,
    /// A session someone opened, whose dialog expired: an ordinary turn, on
    /// the registration that makes a second concurrent send impossible.
    Session {
        /// The turn already registered against the session.
        turn_id: String,
        /// Its token, so a Stop reaches the resumed turn like any other.
        cancel: tokio_util::sync::CancellationToken,
    },
}

impl Resume {
    /// Claims the run, or says why it cannot be picked up now.
    fn claim(state: &AppState, ask: &ParkedAsk) -> AppResult<Self> {
        if !ask.routine_id.is_empty() {
            runner::claim_resume(state, ask)?;
            return Ok(Self::Routine);
        }

        let turn_id = uuid::Uuid::new_v4().to_string();
        let cancel = state.turns().begin(&ask.session_id, &turn_id)?;
        Ok(Self::Session { turn_id, cancel })
    }

    /// Gives the claim back: the answer was not recorded, so nothing runs.
    fn give_up(&self, state: &AppState, ask: &ParkedAsk) {
        match self {
            Self::Routine => state.scheduler().release(&ask.routine_id),
            Self::Session { turn_id, .. } => {
                state
                    .turns()
                    .finish(&ask.session_id, turn_id, SessionState::Idle);
            }
        }
    }

    /// Starts the run the answer belongs to.
    fn go<R: Runtime>(self, app: &AppHandle<R>, ask: &ParkedAsk, decision: Decision) {
        match self {
            Self::Routine => runner::resume_claimed(app, ask, decision),
            Self::Session { turn_id, cancel } => {
                super::session::resume_parked(app.clone(), ask, turn_id, cancel, decision);
            }
        }
    }
}
