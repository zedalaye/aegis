//! Settings commands (PLAN 2.1, "Settings and audit").
//!
//! A key can be written and cleared, never read: [`settings_get`] returns
//! [`MaskedSettings`]. [`settings_probe_provider`] tells a bad address, a bad
//! key and a down server apart. Every change emits `settings:changed`.

use tauri::{AppHandle, Emitter, Runtime, State};

use crate::agent::{ModelCatalog, ProviderProbe};
use crate::error::AppResult;
use crate::secrets::ApiKey;
use crate::state::AppState;
use crate::store::{AuthKind, MaskedSettings};

use super::window::MAIN_WINDOW;

/// The event a settings change announces itself with (PLAN 2.2).
pub const EVENT_SETTINGS_CHANGED: &str = "settings:changed";

/// The current provider settings, with the key masked.
#[tauri::command(rename_all = "snake_case")]
pub fn settings_get(state: State<'_, AppState>) -> AppResult<MaskedSettings> {
    Ok(state.masked_settings())
}

/// Saves the base URL and the model, and the key when one is given.
///
/// A `None` or blank `api_key` keeps the stored key (clearing is
/// [`settings_clear_key`]). Settings are validated and written before the key,
/// so a rejected URL never touches the credential store. The model's output
/// ceiling is looked up here; a failed lookup leaves it unset.
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_set<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    base_url: String,
    model: String,
    api_key: Option<String>,
    auth_kind: Option<AuthKind>,
) -> AppResult<MaskedSettings> {
    let auth_kind = auth_kind.unwrap_or(AuthKind::ApiKey);

    // Asked before the write, and with the key that is about to be stored
    // rather than the one already there: a first-time setup types the address,
    // the model and the key in one go, and a lookup that used the old key
    // would fail on exactly the save that most needs to succeed.
    let cap = state
        .model_output_cap(auth_kind, &base_url, &model, api_key.as_deref())
        .await;

    state.settings().set(&base_url, &model, auth_kind, cap)?;

    if let Some(key) = api_key.as_deref().and_then(ApiKey::new) {
        state.secrets().store(&key)?;
    }

    Ok(announce(&app, state.masked_settings()))
}

/// Removes the stored key.
///
/// From the credential store only; the result may still say `key_source: "env"`.
#[tauri::command(rename_all = "snake_case")]
pub fn settings_clear_key<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
) -> AppResult<MaskedSettings> {
    state.secrets().clear()?;

    Ok(announce(&app, state.masked_settings()))
}

/// Asks the configured server whether it is there and whether the key works.
///
/// Never fails: every outcome is a [`ProviderProbe`].
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_probe_provider(state: State<'_, AppState>) -> AppResult<ProviderProbe> {
    Ok(state.probe_provider().await)
}

/// Lists the models the chosen authentication can use.
///
/// Never fails as a command. A live list is preferred; if the server cannot
/// be asked, the payload carries a fallback and a sentence saying why.
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_list_models(
    state: State<'_, AppState>,
    auth_kind: AuthKind,
    base_url: String,
) -> AppResult<ModelCatalog> {
    Ok(state.list_models(auth_kind, &base_url).await)
}

/// Emits `settings:changed` and hands the payload back to the caller.
///
/// `emit_to` the main window (PLAN 2.2); an emit failure is only logged.
fn announce<R: Runtime>(app: &AppHandle<R>, settings: MaskedSettings) -> MaskedSettings {
    if let Err(err) = app.emit_to(MAIN_WINDOW, EVENT_SETTINGS_CHANGED, &settings) {
        tracing::debug!(%err, "no window to receive {EVENT_SETTINGS_CHANGED}");
    }
    settings
}
