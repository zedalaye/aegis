//! Session and turn commands (PLAN 2.1, "Sessions and turns").
//!
//! Six of the seven commands are thin: look something up, hand it back. The
//! seventh, [`session_send`], is the one with a shape worth reading.
//!
//! It returns as soon as the turn is *registered*, not when it is finished.
//! Everything after that arrives as events (PLAN 2.1). Two reasons, and the
//! second is the important one: a turn can run for minutes, and an `invoke`
//! that outlived the window it was called from would be a promise nothing can
//! resolve; and a turn that is only observable through its return value cannot
//! be cancelled, cannot stream, and cannot be watched by a window that was
//! reopened halfway through.
//!
//! So the ordering inside `session_send` is deliberate. The turn is registered
//! before the message is stored, because registration is what refuses a second
//! concurrent turn — storing first would let two sends race and interleave two
//! users' messages in one transcript. The user's message is stored before the
//! task is spawned, so the transcript the turn reads already contains what it
//! is answering. And if anything between those two steps fails, the
//! registration is undone, or the session would be left running a turn that
//! does not exist.

use tauri::{AppHandle, Emitter, Manager, Runtime, State};

use crate::agent::event::{Event, EventSink};
use crate::agent::turn::{self, Turn, TurnPlan};
use crate::state::AppState;
use crate::store::{Message, SessionDetail, SessionState, SessionSummary, TurnHandle};

use crate::error::AppResult;

use super::window::MAIN_WINDOW;

/// Creates a session in a project.
#[tauri::command(rename_all = "snake_case")]
pub fn session_create(
    state: State<'_, AppState>,
    project_id: String,
    title: Option<String>,
) -> AppResult<SessionSummary> {
    state.sessions().create(&project_id, title.as_deref())
}

/// A project's sessions, most recently active first.
#[tauri::command(rename_all = "snake_case")]
pub fn session_list(
    state: State<'_, AppState>,
    project_id: String,
) -> AppResult<Vec<SessionSummary>> {
    Ok(state.session_list(&project_id))
}

/// One session with its transcript.
#[tauri::command(rename_all = "snake_case")]
pub fn session_open(state: State<'_, AppState>, session_id: String) -> AppResult<SessionDetail> {
    state.session_detail(&session_id)
}

/// Renames a session. An empty title is refused.
#[tauri::command(rename_all = "snake_case")]
pub fn session_rename(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    title: String,
) -> AppResult<()> {
    let summary =
        state
            .sessions()
            .rename(&session_id, &title, state.turns().state_of(&session_id))?;

    WindowSink::new(app).emit(Event::SessionUpdated(summary));
    Ok(())
}

/// Deletes a session and its transcript.
///
/// Everything the process knew about the session goes first: the running turn
/// is cancelled, its open approvals are withdrawn and its grants are dropped.
/// Deleting the transcript out from under a live turn would leave it writing
/// messages into a session that no longer exists — which the turn loop
/// survives, but only by logging a warning per message — and leaving a dialog
/// answerable would leave a button that approves a call nothing will run.
#[tauri::command(rename_all = "snake_case")]
pub fn session_delete(state: State<'_, AppState>, session_id: String) -> AppResult<()> {
    state.close_session(&session_id);
    state.sessions().delete(&session_id)
}

/// Sends a message and starts a turn.
///
/// Returns once the turn is registered; the reply arrives as `turn:*` events.
/// Sending into a session that is already running is refused with
/// `E_TURN_BUSY`.
#[tauri::command(rename_all = "snake_case")]
pub fn session_send(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    text: String,
) -> AppResult<TurnHandle> {
    let turn_id = uuid::Uuid::new_v4().to_string();

    // First, because this is what makes a second concurrent send impossible.
    let cancel = state.turns().begin(&session_id, &turn_id)?;

    let sink = WindowSink::new(app.clone());
    let started = state
        .sessions()
        .append(&session_id, Message::user(text), SessionState::Running)
        .and_then(|summary| {
            sink.emit(Event::SessionUpdated(summary));
            state.workspace_of(&session_id)
        });

    let workspace = match started {
        Ok(workspace) => workspace,
        Err(err) => {
            // The session is gone, or its message could not be stored. Undo
            // the registration: a session left `running` with no task behind
            // it can never be sent to again.
            state
                .turns()
                .finish(&session_id, &turn_id, SessionState::Idle);
            return Err(err);
        }
    };

    let plan = TurnPlan {
        session_id: session_id.clone(),
        turn_id: turn_id.clone(),
        workspace,
    };

    tauri::async_runtime::spawn(async move {
        run_turn(app, plan, cancel).await;
    });

    Ok(TurnHandle {
        session_id,
        turn_id,
    })
}

/// Cancels a running turn.
///
/// Idempotent from the user's side — pressing stop twice is not an error worth
/// reporting — but a handle from a turn that has already finished does fail,
/// so the UI refetches rather than leaving a stop button that does nothing.
#[tauri::command(rename_all = "snake_case")]
pub fn session_cancel(
    state: State<'_, AppState>,
    session_id: String,
    turn_id: String,
) -> AppResult<()> {
    state.turns().cancel(&session_id, &turn_id)
}

/// Drives one turn on the async runtime, and leaves the session drawable
/// however it ends.
///
/// The state is looked up from the handle rather than captured, because a
/// `State<'_, AppState>` borrows the invocation and this outlives it.
async fn run_turn<R: Runtime>(
    app: AppHandle<R>,
    plan: TurnPlan,
    cancel: tokio_util::sync::CancellationToken,
) {
    let Some(state) = app.try_state::<AppState>() else {
        // Only reachable if the application is being torn down.
        tracing::warn!("a turn started with no application state");
        return;
    };

    let sink = WindowSink::new(app.clone());
    let reason = Turn {
        sessions: state.sessions(),
        turns: state.turns(),
        grants: state.grants(),
        approvals: state.approvals(),
        audit: state.audit(),
        provider: state.provider(),
        sink: &sink,
        self_exe: state.self_exe(),
    }
    .run(&plan, &cancel)
    .await;

    // Retire before the final summary is read, so the row the sidebar
    // receives is the one that says the session is no longer running.
    let resting = turn::resting_state(reason);
    state.retire_turn(&plan.session_id, &plan.turn_id, resting);

    if let Some(summary) = turn::summarize(state.sessions(), &plan.session_id, resting) {
        sink.emit(Event::SessionUpdated(summary));
    }
}

/// An [`EventSink`] that emits to the main window.
///
/// `emit_to` rather than `emit`: streaming events go to the one window that
/// asked for them, never as a global broadcast (PLAN 2.2). A failure is logged
/// and swallowed — the window closing mid-turn is ordinary, and a turn that
/// aborted because nobody was watching would lose work for no reason.
pub struct WindowSink<R: Runtime> {
    app: AppHandle<R>,
}

impl<R: Runtime> WindowSink<R> {
    /// Wraps a handle.
    pub const fn new(app: AppHandle<R>) -> Self {
        Self { app }
    }
}

impl<R: Runtime> EventSink for WindowSink<R> {
    fn emit(&self, event: Event) {
        let name = event.name();
        if let Err(err) = self.app.emit_to(MAIN_WINDOW, name, event.payload()) {
            tracing::debug!(%err, event = name, "no window to receive an event");
        }
    }
}
