//! Connectors: the tools this build did not write (PLAN 7.3, Phase 18).
//!
//! List, save, delete, enable, reconnect, plus the boot-time starter. Only a
//! person in Settings reaches these — no tool adds a connector.
//!
//! Nothing here grants tools; that is on the identity (except the built-in
//! Assistant, which holds every tool). Saving writes the record, then starts
//! the process; a failed start is still saved, with stderr on the row.

use tauri::{AppHandle, Emitter as _, Manager as _, Runtime, State};

use crate::agent::event::name::CONNECTOR_UPDATED;
use crate::error::AppResult;
use crate::mcp::ConnectorView;
use crate::state::AppState;
use crate::store::ConnectorDraft;

use super::window::MAIN_WINDOW;

/// Every connector, with what is measured about it right now.
///
/// State, tools, stderr tail and missing env vars, all measured now.
#[tauri::command]
pub fn connector_list(state: State<'_, AppState>) -> Vec<ConnectorView> {
    state.connector_views()
}

/// Creates a connector, or replaces one, and starts it.
///
/// `None` creates; otherwise the row is replaced in place. The process is
/// always restarted; the returned row says why a start failed.
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
/// Allow-lists keep the now-dangling names, which resolve to nothing.
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
/// Nothing restarts on its own (no respawn loop hiding a broken config).
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
/// Also installs the sink that emits `connector:updated`.
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
