//! The settings document: `settings.json`.
//!
//! Two fields, and what is *not* here is the point: the base URL and the model
//! name are stored, the API key is not. The key lives in the OS credential
//! store or in the environment ([`secrets`](crate::secrets)); nothing this
//! module writes to disk is a secret, which is what makes the document safe to
//! open, hand-edit and copy between machines like the other two.
//!
//! Both fields start empty, and empty means something: until the user has
//! named a base URL and a model, turns are answered by the scripted provider
//! of Phase 5 rather than by a network client with nowhere to connect
//! ([`ProviderSettings::is_configured`]). A missing *key* is a different
//! state — that is a configured provider that cannot authenticate, and it is
//! reported as `E_NO_API_KEY` rather than silently answered by the fake.
//!
//! The atomic write, the quarantine and the timestamp format live in the
//! parent module, shared with [`projects`](super::projects) and
//! [`sessions`](super::sessions).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use reqwest::Url;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{quarantine, strip_bom, write_atomic};
use crate::error::{AppError, AppResult};
use crate::secrets::KeySource;

/// Name of the document under the application-data directory.
const SETTINGS_FILE: &str = "settings.json";

/// Schema version of [`SettingsFile`].
///
/// A document carrying anything else is treated exactly like a damaged one:
/// quarantined, not guessed at.
const SCHEMA_VERSION: u32 = 1;

/// The path a base URL must *not* already include.
///
/// The commonest way to get this setting wrong is to paste the endpoint rather
/// than the base, which produces a request to `…/chat/completions/chat/
/// completions` and a 404 that reads like the server is down. Naming the
/// mistake is worth more than tolerating it silently.
const ENDPOINT_SUFFIX: &str = "/chat/completions";

// ---------------------------------------------------------------------------
// IPC payloads (PLAN 2.1, "Settings and audit")
// ---------------------------------------------------------------------------

/// Everything the WebView is allowed to know about the provider settings.
///
/// The name is the contract. There is no unmasked counterpart and no command
/// that returns one: a key can be written and cleared, never read back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct MaskedSettings {
    /// The OpenAI-compatible base URL, normalized. Empty when unset.
    pub base_url: String,
    /// The model id sent with every request. Empty when unset.
    pub model: String,
    /// Which store answered when the key was last looked for.
    pub key_source: KeySource,
    /// A few characters of the key, for recognition. `None` when there is no
    /// key at all.
    pub key_hint: Option<String>,
    /// Whether this machine has a credential store that answered.
    ///
    /// `false` on headless Linux and on a locked keychain; the panel then
    /// explains the environment variable instead of offering to save a key.
    pub keyring_available: bool,
}

// ---------------------------------------------------------------------------
// On-disk shape
// ---------------------------------------------------------------------------

/// The document itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SettingsFile {
    version: u32,
    provider: ProviderSettings,
}

/// The provider settings, as persisted and as the runtime reads them.
///
/// One provider, and deliberately not a singleton on the way to disk: the
/// document nests it under a `provider` key, so a later roster becomes a list
/// beside it rather than a migration of every field (PLAN 7.1).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderSettings {
    /// The OpenAI-compatible base URL, without a trailing slash.
    #[serde(default)]
    pub base_url: String,
    /// The model id.
    #[serde(default)]
    pub model: String,
}

impl ProviderSettings {
    /// Whether these settings name somewhere to send a request.
    ///
    /// The key is not part of the question. A configured provider with no key
    /// must fail loudly — the user asked for a real model and did not get one
    /// — whereas an unconfigured one is a fresh install, where the scripted
    /// provider answering is the documented behaviour rather than a fault.
    pub fn is_configured(&self) -> bool {
        !self.base_url.is_empty() && !self.model.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Accepts a base URL, or says exactly what is wrong with it.
///
/// Returns the normalized form: trimmed, and without the trailing slash that
/// would otherwise produce a double slash in every request. An empty input is
/// an accepted answer meaning "unset", not a failure — that is how the user
/// goes back to the scripted provider.
///
/// The checks are the three that otherwise turn into an unreadable failure
/// much later: a URL that will not parse — which is also how a missing host is
/// caught, since `http:` and `https:` require one — a scheme that is not HTTP,
/// and a base that already carries the endpoint path.
pub fn normalize_base_url(raw: &str) -> AppResult<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Ok(String::new());
    }

    let refuse = |reason: &str| AppError::Settings {
        field: "base URL",
        reason: reason.to_owned(),
    };

    let url = Url::parse(trimmed).map_err(|err| {
        // The near-universal cause is a host with no scheme, and the parser's
        // own words for it ("relative URL without a base") explain nothing to
        // someone who typed `api.openai.com/v1`.
        tracing::debug!(%err, "a base URL would not parse");
        refuse("it is not a URL. It should look like `https://api.openai.com/v1`")
    })?;

    if !matches!(url.scheme(), "http" | "https") {
        return Err(refuse(&format!(
            "`{}:` is not a scheme Aegis can send to. Use `https:`, or `http:` for a \
             server on this machine",
            url.scheme()
        )));
    }
    if url.path().ends_with(ENDPOINT_SUFFIX) {
        return Err(refuse(
            "it already ends in `/chat/completions`. Aegis appends that itself — the base URL stops at `/v1`",
        ));
    }

    // A key sent over plain HTTP crosses the network in the clear. Refusing it
    // outright would block the local servers this setting exists for, so it is
    // allowed and said out loud.
    if url.scheme() == "http" {
        tracing::warn!(
            host = url.host_str().unwrap_or("?"),
            "the base URL is plain HTTP; the API key will be sent unencrypted"
        );
    }

    Ok(trimmed.to_owned())
}

/// Accepts a model id, or says what is wrong with it.
///
/// Only shape is checked. Whether the model exists is the server's answer to
/// give, and guessing at a list here would go stale the week after it shipped.
pub fn normalize_model(raw: &str) -> AppResult<String> {
    let trimmed = raw.trim();

    if trimmed.contains(char::is_whitespace) {
        return Err(AppError::Settings {
            field: "model",
            reason: "a model id has no spaces in it".to_owned(),
        });
    }
    Ok(trimmed.to_owned())
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// The settings store: the values in memory plus the document backing them.
///
/// Same shape as the project store, for the same reason: two fields changed by
/// a human a few times a year are written out whole on every change, so what
/// is on disk always equals what is in memory once a command has returned.
#[derive(Debug)]
pub struct SettingsStore {
    path: PathBuf,
    provider: Mutex<ProviderSettings>,
}

impl SettingsStore {
    /// Loads the settings from `data_dir`.
    ///
    /// Never fails. Unreadable settings start empty and say so in the log,
    /// which means the app boots and answers with the scripted provider rather
    /// than refusing to start over a file the user can delete.
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(SETTINGS_FILE);

        let provider = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<SettingsFile>(strip_bom(&bytes)) {
                Ok(file) if file.version == SCHEMA_VERSION => {
                    tracing::info!(
                        configured = file.provider.is_configured(),
                        "settings loaded"
                    );
                    file.provider
                }
                Ok(file) => {
                    tracing::error!(
                        found = file.version,
                        expected = SCHEMA_VERSION,
                        "unknown settings version"
                    );
                    quarantine(&path);
                    ProviderSettings::default()
                }
                Err(err) => {
                    tracing::error!(%err, "settings are not readable JSON");
                    quarantine(&path);
                    ProviderSettings::default()
                }
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                tracing::info!("no settings yet; no provider is configured");
                ProviderSettings::default()
            }
            Err(err) => {
                tracing::error!(%err, "could not read the settings");
                ProviderSettings::default()
            }
        };

        Self {
            path,
            provider: Mutex::new(provider),
        }
    }

    /// Locks the values.
    ///
    /// A poisoned mutex means another command panicked mid-write. The value
    /// behind it is two strings replaced wholesale and cannot be torn, so
    /// recovering it beats propagating a panic into every later command.
    fn provider(&self) -> MutexGuard<'_, ProviderSettings> {
        self.provider
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The current provider settings.
    pub fn get(&self) -> ProviderSettings {
        self.provider().clone()
    }

    /// Replaces the provider settings, after normalizing both fields.
    ///
    /// Validation happens before the lock is taken and before anything is
    /// written, so a rejected base URL leaves the previous settings exactly as
    /// they were — a user correcting a typo does not lose their model name.
    pub fn set(&self, base_url: &str, model: &str) -> AppResult<ProviderSettings> {
        let next = ProviderSettings {
            base_url: normalize_base_url(base_url)?,
            model: normalize_model(model)?,
        };

        let mut provider = self.provider();
        provider.clone_from(&next);
        self.save(&provider)?;

        Ok(next)
    }

    /// Writes the document.
    fn save(&self, provider: &ProviderSettings) -> AppResult<()> {
        let file = SettingsFile {
            version: SCHEMA_VERSION,
            provider: provider.clone(),
        };

        let bytes = serde_json::to_vec_pretty(&file).map_err(|err| {
            tracing::error!(%err, "settings would not serialize");
            AppError::Settings {
                field: "settings",
                reason: "they could not be written".to_owned(),
            }
        })?;

        write_atomic(&self.path, &bytes).map_err(|err| {
            tracing::error!(%err, path = %self.path.display(), "could not save the settings");
            AppError::Store {
                action: "save",
                source: err,
            }
        })
    }

    /// Where the document lives. For diagnostics and tests.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    #[test]
    fn a_trailing_slash_is_removed_so_requests_carry_one() {
        assert_eq!(
            normalize_base_url("https://api.openai.com/v1/").expect("accepted"),
            "https://api.openai.com/v1"
        );
        assert_eq!(
            normalize_base_url("  https://api.openai.com/v1  ").expect("accepted"),
            "https://api.openai.com/v1"
        );
    }

    /// Emptying the field is how a user goes back to the scripted provider, so
    /// it is an answer rather than a validation failure.
    #[test]
    fn an_empty_base_url_is_accepted_as_unset() {
        assert_eq!(normalize_base_url("").expect("accepted"), "");
        assert_eq!(normalize_base_url("   ").expect("accepted"), "");
    }

    #[test]
    fn a_local_server_over_plain_http_is_allowed() {
        assert_eq!(
            normalize_base_url("http://127.0.0.1:11434/v1").expect("accepted"),
            "http://127.0.0.1:11434/v1"
        );
    }

    /// The failures worth naming, each carrying the message the user needs
    /// rather than the parser's.
    #[test]
    fn a_base_url_that_cannot_work_is_refused_with_a_reason() {
        let cases = [
            ("api.openai.com/v1", "should look like"),
            ("ftp://example.com", "not a scheme"),
            ("https://", "should look like"),
            (
                "https://api.openai.com/v1/chat/completions",
                "appends that itself",
            ),
        ];

        for (input, expected) in cases {
            let err = normalize_base_url(input).expect_err(input).to_string();
            assert!(err.contains(expected), "{input} gave `{err}`");
        }
    }

    #[test]
    fn a_model_id_is_trimmed_and_must_be_one_word() {
        assert_eq!(
            normalize_model("  gpt-4o-mini \n").expect("accepted"),
            "gpt-4o-mini"
        );
        assert!(normalize_model("gpt 4o mini").is_err());
    }

    /// The key is the one thing that must never be in this file. A user who
    /// opens `settings.json` should find nothing worth protecting.
    #[test]
    fn the_document_never_contains_a_secret() {
        let dir = TempDir::new().expect("temp dir");
        let store = SettingsStore::load(dir.path());

        store
            .set("https://api.openai.com/v1", "gpt-4o-mini")
            .expect("accepted");

        let written = fs::read_to_string(store.path()).expect("the document");
        assert!(written.contains("api.openai.com"));
        assert!(written.contains("gpt-4o-mini"));
        assert!(
            !written.to_lowercase().contains("key"),
            "the settings document mentions a key: {written}"
        );
    }

    #[test]
    fn settings_survive_a_restart() {
        let dir = TempDir::new().expect("temp dir");

        SettingsStore::load(dir.path())
            .set("https://example.test/v1", "some-model")
            .expect("accepted");

        let reopened = SettingsStore::load(dir.path()).get();
        assert_eq!(reopened.base_url, "https://example.test/v1");
        assert_eq!(reopened.model, "some-model");
        assert!(reopened.is_configured());
    }

    /// A rejected value must not take the accepted one with it: the user is
    /// mid-correction, and losing the other field is how a settings panel
    /// makes a typo expensive.
    #[test]
    fn a_rejected_change_leaves_the_previous_settings_intact() {
        let dir = TempDir::new().expect("temp dir");
        let store = SettingsStore::load(dir.path());

        store
            .set("https://example.test/v1", "some-model")
            .expect("accepted");
        store.set("not a url", "some-model").expect_err("refused");

        assert_eq!(store.get().base_url, "https://example.test/v1");
    }

    #[test]
    fn a_provider_is_configured_only_once_both_fields_are_set() {
        let mut provider = ProviderSettings::default();
        assert!(!provider.is_configured(), "a fresh install");

        provider.base_url = "https://api.openai.com/v1".to_owned();
        assert!(!provider.is_configured(), "a URL with no model");

        provider.model = "gpt-4o-mini".to_owned();
        assert!(provider.is_configured());
    }

    /// A document from a future version is quarantined rather than guessed at,
    /// and the app still starts.
    #[test]
    fn an_unknown_version_is_quarantined() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(SETTINGS_FILE);
        fs::write(
            &path,
            br#"{"version":99,"provider":{"base_url":"https://x.test","model":"m"}}"#,
        )
        .expect("write");

        let store = SettingsStore::load(dir.path());

        assert_eq!(store.get(), ProviderSettings::default());
        assert!(!path.exists(), "the damaged document was moved aside");
    }
}
