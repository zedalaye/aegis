//! Connectors: the tools this build did not write (PLAN 7.3, Phase 18).
//!
//! Five commands — list, save, delete, enable, reconnect — and one background
//! task that starts what is enabled when the process boots.
//!
//! What is deliberately *not* here is the point of the module. There is no
//! tool that adds a connector, and there is no command a model can reach:
//! adding one names a program to start, and a model that could name a program
//! to start would have `shell_exec` with no dialog in front of it. Every
//! command below is a person in Settings.
//!
//! There is also no command that grants a connector's tools to anybody.
//! Installing a connector makes its tools *exist*; who may call them is edited
//! on the identity, in the panel above this one, because that is the same act
//! for a connector's tool as for `shell_exec` (AGENTS.md: "granting it to an
//! identity is a separate act"). The one exception is the built-in Assistant,
//! whose allow-list has never been a list somebody wrote — it is "every tool
//! this build has", and a connector the operator installed is one of those.
//!
//! Saving is where the two halves meet. The record is written first and the
//! process is started second, and a start that fails is still a saved record:
//! a person who mistyped a package name needs the row to come back with the
//! server's own stderr on it, not a rejected form that forgets what they typed.

use tauri::{AppHandle, Emitter as _, Manager as _, Runtime, State};

use crate::agent::event::name::CONNECTOR_UPDATED;
use crate::error::AppResult;
use crate::mcp::ConnectorView;
use crate::state::AppState;
use crate::store::ConnectorDraft;

use super::window::MAIN_WINDOW;

/// Every connector, with what is measured about it right now.
///
/// The state, the tools, the last lines of stderr and the environment
/// variables it names but this process does not hold are all measured on the
/// way out. None of it is stored, because none of it survives a restart.
#[tauri::command]
pub fn connector_list(state: State<'_, AppState>) -> Vec<ConnectorView> {
    state.connector_views()
}

/// Creates a connector, or replaces one, and starts it.
///
/// `connector_id` of `None` creates; otherwise that row is replaced, keeping
/// its place in the list. The connection is rebuilt either way — a command or
/// an argument that changed is a different program, and leaving the old process
/// running under the new record is the one state where the panel would be
/// lying.
///
/// The result is the row, not an acknowledgement: a connector that would not
/// start is a row that says why.
#[tauri::command(rename_all = "snake_case")]
pub async fn connector_save(
    state: State<'_, AppState>,
    connector_id: Option<String>,
    draft: ConnectorDraft,
) -> AppResult<ConnectorView> {
    let saved = state
        .connector_store()
        .save(connector_id.as_deref(), &draft)?;

    // The id is editable, so a rename leaves the old process running under a
    // name nothing refers to any more. Stopped explicitly rather than left to
    // the roster, which is keyed on the id and would never look at it again.
    if let Some(previous) = connector_id.as_deref() {
        if previous != saved.id {
            state.connectors().disconnect(previous).await;
        }
    }

    if saved.enabled {
        Ok(state.connectors().connect(&saved).await)
    } else {
        state.connectors().disconnect(&saved.id).await;
        Ok(state.connector_view(&saved))
    }
}

/// Deletes a connector and stops its process.
///
/// The identities that were granted its tools keep those names. They are
/// refused anyway — the catalog no longer carries them, so the model is not
/// offered them and policy has nothing to resolve — and rewriting somebody's
/// allow-list because a row was deleted is a change nobody asked for.
#[tauri::command(rename_all = "snake_case")]
pub async fn connector_delete(state: State<'_, AppState>, connector_id: String) -> AppResult<()> {
    state.connector_store().delete(&connector_id)?;
    state.connectors().disconnect(&connector_id).await;
    Ok(())
}

/// Starts or stops a connector without editing it.
#[tauri::command(rename_all = "snake_case")]
pub async fn connector_set_enabled(
    state: State<'_, AppState>,
    connector_id: String,
    enabled: bool,
) -> AppResult<ConnectorView> {
    let updated = state
        .connector_store()
        .set_enabled(&connector_id, enabled)?;

    if enabled {
        Ok(state.connectors().connect(&updated).await)
    } else {
        state.connectors().disconnect(&updated.id).await;
        Ok(state.connector_view(&updated))
    }
}

/// Starts a connector again after it failed or stopped.
///
/// The button exists because nothing retries on its own. A server that died is
/// left dead and the row says so — "two failures → human" (PLAN 7.4) applied to
/// a process, and the alternative is a respawn loop that hides a broken
/// configuration behind a connector that is up for four seconds at a time.
#[tauri::command(rename_all = "snake_case")]
pub async fn connector_reconnect(
    state: State<'_, AppState>,
    connector_id: String,
) -> AppResult<ConnectorView> {
    let connector = state.connector_store().get(&connector_id)?;
    Ok(state.connectors().connect(&connector).await)
}

/// Starts every enabled connector, in the background, once at boot.
///
/// Also installs the sink a change of state is announced on, which is what
/// makes a row fill in by itself rather than only when somebody opens the
/// panel: a connector can come up a minute after the window did, and it can
/// die at three in the morning.
pub fn spawn<R: Runtime>(app: AppHandle<R>) {
    tauri::async_runtime::spawn(async move {
        let Some(state) = app.try_state::<AppState>() else {
            tracing::warn!("no state to start connectors from");
            return;
        };

        let announcer = app.clone();
        state
            .connectors()
            .announce_on(std::sync::Arc::new(move |view: ConnectorView| {
                // `emit_to`, like every other event: one window asked for
                // these, and a closed window is ordinary rather than a fault.
                if let Err(err) = announcer.emit_to(MAIN_WINDOW, CONNECTOR_UPDATED, &view) {
                    tracing::debug!(%err, "no window to receive a connector update");
                }
            }));

        let configured = state.connector_store().enabled();
        if configured.is_empty() {
            return;
        }
        tracing::info!(count = configured.len(), "starting connectors");
        state.connectors().connect_all(&configured).await;
    });
}
