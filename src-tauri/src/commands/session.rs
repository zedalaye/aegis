//! Session and turn commands (PLAN 2.1, "Sessions and turns").
//!
//! [`session_send`] returns once the turn is registered; the rest is events
//! (PLAN 2.1), so turns stream, cancel and survive a reopened window.
//!
//! Its order matters: register (refusing a concurrent turn), then store the
//! message, then spawn — undoing the registration if a step in between fails.

use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager, Runtime, State};

use crate::agent::event::{Event, EventSink};
use crate::agent::turn::{self, Standing, Turn, TurnPlan};
use crate::handoff::bus;
use crate::handoff::runner::AppRunner;
use crate::state::AppState;
use crate::store::{Message, SessionDetail, SessionState, SessionSummary, TurnHandle};

use crate::error::AppResult;

use super::window::MAIN_WINDOW;

/// Creates a session in a project, as an identity.
///
/// An omitted `agent_id` is the built-in identity; the binding is permanent
/// ([`SessionStore::create`](crate::store::SessionStore::create)).
#[tauri::command(rename_all = "snake_case")]
pub fn session_create(
    state: State<'_, AppState>,
    project_id: String,
    title: Option<String>,
    agent_id: Option<String>,
) -> AppResult<SessionSummary> {
    state.create_session(&project_id, title.as_deref(), agent_id.as_deref())
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
/// Cancels the turn and drops approvals and grants before deleting the
/// transcript.
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
        // Read once, here, with the workspace: the project owns both, and a
        // turn that re-read either mid-round could describe one machine to the
        // model and run its commands on another (PLAN 7.12).
        exec_host: state.exec_host_of(&session_id),
    };

    tauri::async_runtime::spawn(async move {
        run_turn(app, plan, cancel).await;
    });

    Ok(TurnHandle {
        session_id,
        turn_id,
    })
}

/// Folds this session's older turns into state (PLAN 7.3, Phase 14).
///
/// A forced fold, keeping the usual raw tail; returns the session either way.
/// `E_TURN_BUSY` while a turn runs. Nothing is deleted.
#[tauri::command(rename_all = "snake_case")]
pub fn session_compact(state: State<'_, AppState>, session_id: String) -> AppResult<SessionDetail> {
    state.compact_session(&session_id)
}

/// Cancels a running turn.
///
/// A handle from a finished turn fails, so the UI refetches.
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
    // Both resolved now, not at startup and not per round: the settings and the
    // key are read for this turn (see `AppState::provider_for`), and so is the
    // identity — an identity edited between two messages should reach the next
    // turn whole, rather than halfway through one.
    let agent = state.agent_of(&plan.session_id);
    let provider = state.provider_for(&agent);

    // Per-turn delegation state (Phase 15); none without a workspace, since a
    // team needs shared files (`COS.md` *Memory*).
    let bus: Option<Arc<dyn bus::Runner>> =
        state
            .sessions()
            .project_of(&plan.session_id)
            .ok()
            .map(|project_id| {
                Arc::new(AppRunner::new(
                    app.clone(),
                    project_id,
                    plan.session_id.clone(),
                    plan.workspace.clone(),
                    plan.exec_host.clone(),
                )) as Arc<dyn bus::Runner>
            });

    let reason = Turn {
        agent: &agent,
        sessions: state.sessions(),
        turns: state.turns(),
        grants: state.grants(),
        approvals: state.approvals(),
        audit: state.audit(),
        provider: provider.as_ref(),
        sink: &sink,
        self_exe: state.self_exe(),
        captures: state.captures(),
        skills: state.skills(),
        memories: state.memories(),
        connectors: state.connectors(),
        standing: Standing::Own(bus.as_ref()),
        unattended: None,
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
/// `emit_to` the main window, never a broadcast (PLAN 2.2). Failures are
/// logged: a closed window must not abort the turn.
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
