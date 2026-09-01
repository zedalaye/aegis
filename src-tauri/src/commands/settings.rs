//! Settings commands (PLAN 2.1, "Settings and audit").
//!
//! Four commands over one provider configuration, and the shape of the set is
//! the security property: a key can be *written* and *cleared*, never read.
//! There is no `settings_get_key`, and [`settings_get`] answers with a
//! [`MaskedSettings`] — a source and four characters — because a command that
//! could return the key would put the key in the WebView, which is the one
//! thing `AGENTS.md` says must never happen.
//!
//! [`settings_probe_provider`] exists for the same reason the audit log has a
//! path command: a configuration you cannot test is a configuration you argue
//! with. It separates the three failures that look identical from a chat
//! window — the address is wrong, the key is wrong, the server is down.
//!
//! Every change emits `settings:changed` to the main window, so a panel open
//! in one place and a change made in another do not disagree.

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
/// `api_key` is `None` for the ordinary case — a user changing their model
/// does not retype their key — and an all-whitespace value is treated the same
/// way, because a form field that was focused and left alone must not clear a
/// credential. Removing a key is [`settings_clear_key`], which says so.
///
/// The two halves are ordered deliberately: the settings are validated and
/// written first, and the key only afterwards. A rejected base URL therefore
/// leaves the credential store untouched, and a credential store that refuses
/// the write leaves settings the user can still correct — neither failure
/// leaves half a configuration behind that the panel does not show.
#[tauri::command(rename_all = "snake_case")]
pub fn settings_set<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    base_url: String,
    model: String,
    api_key: Option<String>,
    auth_kind: Option<AuthKind>,
) -> AppResult<MaskedSettings> {
    state
        .settings()
        .set(&base_url, &model, auth_kind.unwrap_or(AuthKind::ApiKey))?;

    if let Some(key) = api_key.as_deref().and_then(ApiKey::new) {
        state.secrets().store(&key)?;
    }

    Ok(announce(&app, state.masked_settings()))
}

/// Removes the stored key.
///
/// Only the one in the credential store. A key in the environment is the
/// process's own inheritance and Aegis does not edit it, so the answer this
/// returns may still report `key_source: "env"` — which is the honest result
/// and is exactly what the panel needs to say so.
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
/// Never fails as a command. Everything it can find out — unreachable, 401,
/// 404, nothing configured at all — is a [`ProviderProbe`] describing what
/// happened, because "the probe failed" is not a useful thing to hand someone
/// who pressed a button to find out what is wrong.
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
/// `emit_to` rather than `emit`: like the streaming events, this goes to the
/// one window that renders it and not out as a global broadcast (PLAN 2.2). A
/// failure is logged and swallowed — the command itself succeeded, and the
/// caller already has the new value in its own reply.
fn announce<R: Runtime>(app: &AppHandle<R>, settings: MaskedSettings) -> MaskedSettings {
    if let Err(err) = app.emit_to(MAIN_WINDOW, EVENT_SETTINGS_CHANGED, &settings) {
        tracing::debug!(%err, "no window to receive {EVENT_SETTINGS_CHANGED}");
    }
    settings
}
