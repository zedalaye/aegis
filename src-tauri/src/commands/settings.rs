//! Settings commands (PLAN 2.1, "Settings and audit"; PLAN 7.19).
//!
//! A key can be written and cleared, never read: [`settings_get`] returns
//! [`MaskedSettings`], the roster with every key masked.
//! [`settings_probe_provider`] tells a bad address, a bad key and a down
//! server apart. Every change emits `settings:changed`.

use tauri::{AppHandle, Emitter, Runtime, State};

use crate::agent::{ModelCatalog, ProviderProbe};
use crate::error::AppResult;
use crate::state::AppState;
use crate::store::{AuthKind, MaskedSettings, RowDraft, DEFAULT_PROVIDER_ID};

use super::window::MAIN_WINDOW;

/// The event a settings change announces itself with (PLAN 2.2).
pub const EVENT_SETTINGS_CHANGED: &str = "settings:changed";

/// Every provider row, with keys masked.
#[tauri::command(rename_all = "snake_case")]
pub fn settings_get(state: State<'_, AppState>) -> AppResult<MaskedSettings> {
    Ok(state.masked_settings())
}

/// Saves one row's base URL, model and label, and its key when one is given.
///
/// `provider_id` defaults to the default row; an unknown id is refused. A
/// `None` or blank `api_key` keeps the stored key (clearing is
/// [`settings_clear_key`]); a `None` label keeps the stored one.
#[tauri::command(rename_all = "snake_case")]
#[allow(clippy::too_many_arguments)]
pub async fn settings_set<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    base_url: String,
    model: String,
    api_key: Option<String>,
    auth_kind: Option<AuthKind>,
    provider_id: Option<String>,
    label: Option<String>,
) -> AppResult<MaskedSettings> {
    let provider_id = provider_id.as_deref().unwrap_or(DEFAULT_PROVIDER_ID);
    let draft = RowDraft {
        label: label.as_deref(),
        base_url: &base_url,
        model: &model,
        auth_kind: auth_kind.unwrap_or_default(),
        max_output_tokens: None,
    };

    state
        .save_provider(provider_id, &draft, api_key.as_deref())
        .await?;

    Ok(announce(&app, state.masked_settings()))
}

/// Appends a provider row under a fresh id, and its key when one is given.
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_add_provider<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    base_url: String,
    model: String,
    auth_kind: AuthKind,
    label: Option<String>,
    api_key: Option<String>,
) -> AppResult<MaskedSettings> {
    let draft = RowDraft {
        label: label.as_deref(),
        base_url: &base_url,
        model: &model,
        auth_kind,
        max_output_tokens: None,
    };

    state.add_provider(&draft, api_key.as_deref()).await?;

    Ok(announce(&app, state.masked_settings()))
}

/// Deletes a provider row.
///
/// Refused for the default row, and while an identity or a session override
/// names it — never cascaded.
#[tauri::command(rename_all = "snake_case")]
pub fn settings_delete_provider<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    provider_id: String,
) -> AppResult<MaskedSettings> {
    state.delete_provider(&provider_id)?;

    Ok(announce(&app, state.masked_settings()))
}

/// Removes one row's stored key.
///
/// From the credential store only; the default row may still say
/// `key_source: "env"`. Refused for a CLI row.
#[tauri::command(rename_all = "snake_case")]
pub fn settings_clear_key<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    provider_id: Option<String>,
) -> AppResult<MaskedSettings> {
    state.clear_provider_key(provider_id.as_deref().unwrap_or(DEFAULT_PROVIDER_ID))?;

    Ok(announce(&app, state.masked_settings()))
}

/// Asks one row's server whether it is there and whether the key works.
///
/// Every reachable outcome is a [`ProviderProbe`]; only an unknown row fails.
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_probe_provider(
    state: State<'_, AppState>,
    provider_id: Option<String>,
) -> AppResult<ProviderProbe> {
    state
        .probe_provider(provider_id.as_deref().unwrap_or(DEFAULT_PROVIDER_ID))
        .await
}

/// Lists the models the chosen authentication can use.
///
/// Never fails as a command. A live list is preferred; if the server cannot
/// be asked, the payload carries a fallback and a sentence saying why.
/// `provider_id` picks whose stored key asks; it defaults to the default row.
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_list_models(
    state: State<'_, AppState>,
    auth_kind: AuthKind,
    base_url: String,
    provider_id: Option<String>,
) -> AppResult<ModelCatalog> {
    Ok(state
        .list_models(
            provider_id.as_deref().unwrap_or(DEFAULT_PROVIDER_ID),
            auth_kind,
            &base_url,
        )
        .await)
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
