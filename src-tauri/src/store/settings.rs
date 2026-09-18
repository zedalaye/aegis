//! The settings document: `settings.json`.
//!
//! A roster of chat provider rows (PLAN 7.19): base URL, model and auth kind —
//! never the key, which lives in the OS credential store, the environment or a
//! CLI login ([`secrets`](crate::secrets), [`oauth`](crate::oauth)). The row
//! [`DEFAULT_PROVIDER_ID`] always exists, and comes first.
//!
//! An unconfigured row means the scripted provider
//! ([`ProviderSettings::is_configured`]); a configured row without a key is
//! `E_NO_API_KEY`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use reqwest::Url;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use serde_json::{Map, Value};
use uuid::Uuid;

use super::agents::DEFAULT_PROVIDER_ID;
use super::{quarantine, strip_bom, write_atomic};
use crate::error::{AppError, AppResult};
use crate::secrets::KeySource;

/// Name of the document under the application-data directory.
const SETTINGS_FILE: &str = "settings.json";

/// Schema version of [`SettingsFile`]; any other version is quarantined.
const SCHEMA_VERSION: u32 = 1;

/// Most provider rows one machine may hold.
pub const PROVIDERS_MAX: usize = 16;

/// Longest row label.
const LABEL_MAX_CHARS: usize = 48;

/// The path a base URL must *not* already include.
///
/// Pasting the endpoint instead of the base is refused with a clear message.
const ENDPOINT_SUFFIX: &str = "/chat/completions";

// ---------------------------------------------------------------------------
// IPC payloads (PLAN 2.1, "Settings and audit")
// ---------------------------------------------------------------------------

/// How a configured provider authenticates.
///
/// Names a credential source, never a token. `api_key`: keyring or
/// `AEGIS_API_KEY` for an OpenAI-compatible host; [`AuthKind::Gemini`]: same
/// store, Google's API; CLI variants reuse an existing login.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum AuthKind {
    /// A key stored in the OS credential store or `AEGIS_API_KEY`.
    #[default]
    ApiKey,
    /// Google AI Studio key, same store, talking to
    /// `generativelanguage.googleapis.com`.
    Gemini,
    /// Claude Code: `~/.claude/.credentials.json` / Keychain.
    ClaudeCli,
    /// OpenAI Codex CLI: `~/.codex/auth.json`.
    CodexCli,
    /// Grok CLI: `~/.grok/auth.json`.
    GrokCli,
}

impl AuthKind {
    /// Whether this kind reads a CLI login instead of an API key.
    pub const fn is_cli(self) -> bool {
        matches!(self, Self::ClaudeCli | Self::CodexCli | Self::GrokCli)
    }

    /// The endpoint this kind talks to when the user has not overridden it.
    pub const fn default_base_url(self) -> &'static str {
        match self {
            Self::ApiKey => "https://api.openai.com/v1",
            Self::Gemini => "https://generativelanguage.googleapis.com/v1beta",
            Self::ClaudeCli => "https://api.anthropic.com",
            Self::CodexCli => "https://chatgpt.com/backend-api/codex",
            Self::GrokCli => "https://cli-chat-proxy.grok.com/v1",
        }
    }

    /// A model id that kind will accept, used to prefill Settings.
    pub const fn default_model(self) -> &'static str {
        match self {
            Self::ApiKey => "",
            Self::Gemini => "gemini-2.5-flash",
            Self::ClaudeCli => "claude-sonnet-4-6",
            Self::CodexCli => "gpt-5.5",
            Self::GrokCli => "grok-4",
        }
    }

    /// Every authentication kind, with the URL and model the form prefills.
    pub fn presets() -> [AuthPreset; 5] {
        [
            Self::ApiKey.preset(),
            Self::Gemini.preset(),
            Self::ClaudeCli.preset(),
            Self::CodexCli.preset(),
            Self::GrokCli.preset(),
        ]
    }

    fn preset(self) -> AuthPreset {
        AuthPreset {
            auth_kind: self,
            default_base_url: self.default_base_url().to_owned(),
            default_model: self.default_model().to_owned(),
        }
    }
}

/// The URL and model Settings prefills for one [`AuthKind`].
///
/// Used when the user switches authentication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct AuthPreset {
    /// Which login this row describes.
    pub auth_kind: AuthKind,
    /// Endpoint used when the base URL field is left empty.
    pub default_base_url: String,
    /// Model id prefilled when the field is empty or still the previous kind's
    /// default.
    pub default_model: String,
}

/// Everything the WebView is allowed to know about the provider roster.
///
/// No unmasked counterpart exists: a key is written or cleared, never read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct MaskedSettings {
    /// Every row, [`DEFAULT_PROVIDER_ID`] first.
    pub providers: Vec<MaskedProvider>,
    /// Whether this machine has a credential store that answered.
    ///
    /// `false` on headless Linux or a locked keychain.
    pub keyring_available: bool,
    /// Prefill values for every authentication kind, so switching in the form
    /// can fill the matching URL and model without a second round trip.
    pub presets: Vec<AuthPreset>,
    /// The decision model (PLAN 7.18), which is not a provider row.
    pub decision: MaskedDecision,
}

/// The TypeSafe decision client's settings, as the WebView may see them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct MaskedDecision {
    /// Which store answered for `typesafe-api-key`.
    pub key_source: KeySource,
    /// A few characters of the key, or `None` without one.
    pub key_hint: Option<String>,
    /// The model as stored. Empty means [`DECISION_DEFAULT_MODEL`].
    pub model: String,
    /// The origin as stored. Empty means [`DECISION_DEFAULT_BASE_URL`].
    pub base_url: String,
    /// Whether a configured key annotates approval dialogs.
    pub annotate_approvals: bool,
    /// What an empty model resolves to.
    pub default_model: String,
    /// What an empty base URL resolves to.
    pub default_base_url: String,
}

/// One provider row, as the WebView may see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct MaskedProvider {
    /// [`DEFAULT_PROVIDER_ID`], or a UUID v4 minted on add.
    pub id: String,
    /// Display name. May be empty.
    pub label: String,
    /// How this provider authenticates.
    pub auth_kind: AuthKind,
    /// The base URL, normalized. Empty when unset.
    pub base_url: String,
    /// The model id sent when nothing overrides it. Empty when unset.
    pub model: String,
    /// The output ceiling the provider's catalog reported for that model, or
    /// `None` for an endpoint that does not publish one.
    ///
    /// Shown so a failed lookup (and a low default cap) is visible.
    #[ts(type = "number | null")]
    pub max_output_tokens: Option<u32>,
    /// Which store answered when the key was last looked for.
    pub key_source: KeySource,
    /// A few characters of the key, for recognition. `None` when there is no
    /// key at all.
    pub key_hint: Option<String>,
}

// ---------------------------------------------------------------------------
// On-disk shape
// ---------------------------------------------------------------------------

/// The document itself.
///
/// `provider` is the singleton written before PLAN 7.19: read, never written.
/// Keys this build does not know land in `rest` and are written back, so a
/// save never drops another half of the document.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SettingsFile {
    version: u32,
    #[serde(default)]
    providers: Vec<ProviderEntry>,
    #[serde(default, skip_serializing)]
    provider: Option<ProviderSettings>,
    #[serde(default)]
    decision: DecisionSettings,
    #[serde(flatten)]
    rest: Map<String, Value>,
}

/// The model a decision request names when the setting is empty.
pub const DECISION_DEFAULT_MODEL: &str = "jev-latest";

/// The origin decision requests go to when the setting is empty.
pub const DECISION_DEFAULT_BASE_URL: &str = "https://api.typesafe.ai";

/// The path a decision base URL must not already include.
const DECISION_ENDPOINT_SUFFIX: &str = "/v1/systemone";

/// The decision client's settings (PLAN 7.18), persisted as `decision`.
/// Empty strings resolve at use time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DecisionSettings {
    /// The Jev model; empty is [`DECISION_DEFAULT_MODEL`].
    pub model: String,
    /// The origin; empty is [`DECISION_DEFAULT_BASE_URL`].
    pub base_url: String,
    /// Whether a configured key annotates approval dialogs (`tool_risk`).
    pub annotate_approvals: bool,
}

impl Default for DecisionSettings {
    fn default() -> Self {
        Self {
            model: String::new(),
            base_url: String::new(),
            annotate_approvals: true,
        }
    }
}

impl DecisionSettings {
    /// The model a request names.
    pub fn resolved_model(&self) -> &str {
        if self.model.is_empty() {
            DECISION_DEFAULT_MODEL
        } else {
            &self.model
        }
    }

    /// The origin a request goes to.
    pub fn resolved_base_url(&self) -> &str {
        if self.base_url.is_empty() {
            DECISION_DEFAULT_BASE_URL
        } else {
            &self.base_url
        }
    }
}

/// Accepts a decision origin, or says what is wrong with it: the rules of
/// [`normalize_base_url`], and the endpoint path is refused because the client
/// appends it.
pub fn normalize_decision_base_url(raw: &str) -> AppResult<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    let refuse = |reason: String| AppError::Settings {
        field: "decision base URL",
        reason,
    };
    if trimmed.ends_with(DECISION_ENDPOINT_SUFFIX) {
        return Err(refuse(
            "it already ends in `/v1/systemone`. Aegis appends that itself — give the origin, \
             like `https://api.typesafe.ai`"
                .to_owned(),
        ));
    }
    normalize_base_url(trimmed).map_err(|err| match err {
        AppError::Settings { reason, .. } => {
            refuse(reason.replace("https://api.openai.com/v1", DECISION_DEFAULT_BASE_URL))
        }
        other => other,
    })
}

/// One row of the roster, as persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderEntry {
    /// [`DEFAULT_PROVIDER_ID`], or a UUID v4.
    pub id: String,
    /// Display name. May be empty.
    #[serde(default)]
    pub label: String,
    /// Where requests go, and as which model.
    #[serde(flatten)]
    pub settings: ProviderSettings,
}

impl ProviderEntry {
    /// The row a fresh install, or an unreadable document, starts with.
    fn unconfigured_default() -> Self {
        Self {
            id: DEFAULT_PROVIDER_ID.to_owned(),
            label: String::new(),
            settings: ProviderSettings::default(),
        }
    }

    /// Whether this is the row that cannot be deleted.
    pub fn is_default(&self) -> bool {
        self.id == DEFAULT_PROVIDER_ID
    }
}

/// One row's connection settings, as persisted and as the runtime reads them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderSettings {
    /// How to authenticate. Missing in documents written before this field
    /// existed, which serde treats as [`AuthKind::ApiKey`].
    #[serde(default)]
    pub auth_kind: AuthKind,
    /// The OpenAI-compatible base URL, without a trailing slash.
    ///
    /// Unused when [`AuthKind`] is a CLI login or Gemini, unless the user
    /// overrode the default endpoint.
    #[serde(default)]
    pub base_url: String,
    /// The model id.
    #[serde(default)]
    pub model: String,
    /// The largest reply this model will produce, from the provider's own
    /// catalog when it was asked (see
    /// [`catalog::output_cap`](crate::agent::provider::catalog::output_cap)).
    ///
    /// Looked up on save, not per turn. `None` leaves the ceiling unset.
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
}

impl ProviderSettings {
    /// Whether these settings name somewhere to send a request.
    ///
    /// The key is not considered, so a keyless configured provider fails loudly.
    /// CLI logins and Gemini need only a model id.
    pub fn is_configured(&self) -> bool {
        if self.model.is_empty() {
            return false;
        }
        !matches!(self.auth_kind, AuthKind::ApiKey) || !self.base_url.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Resolution (PLAN 7.19)
// ---------------------------------------------------------------------------

/// What asks for a provider: an identity's default pair, and a session's
/// override of it. Empty and `None` mean "inherit".
#[derive(Debug, Clone, Copy, Default)]
pub struct BindingRequest<'a> {
    /// The identity's `provider_id`.
    pub identity_provider: &'a str,
    /// The identity's `model`; empty uses the row's.
    pub identity_model: &'a str,
    /// The session's override of the row.
    pub session_provider: Option<&'a str>,
    /// The session's override of the model.
    pub session_model: Option<&'a str>,
}

/// The row a turn answers from, and the settings it sends with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// The row that answered — [`DEFAULT_PROVIDER_ID`] when the named one is
    /// missing.
    pub provider_id: String,
    /// The row's settings with the resolved model in place.
    pub settings: ProviderSettings,
}

/// Picks the row and model for one turn (PLAN 7.19). `rows` holds the default
/// row first, as [`SettingsStore::list`] returns it.
///
/// Row: session, else identity, else default. Model: session, else the row's
/// when the session chose the row, else the identity's, else the row's. A row
/// that is not on file answers from the default row, as it stands. The
/// catalog ceiling is kept only while the model is the row's own.
pub fn resolve(rows: &[ProviderEntry], request: &BindingRequest<'_>) -> Binding {
    let session_provider = request.session_provider.filter(|id| !id.is_empty());
    let session_model = request.session_model.filter(|model| !model.is_empty());
    let wanted = session_provider
        .or(Some(request.identity_provider).filter(|id| !id.is_empty()))
        .unwrap_or(DEFAULT_PROVIDER_ID);

    let fallback = || {
        rows.iter()
            .find(|entry| entry.is_default())
            .cloned()
            .unwrap_or_else(ProviderEntry::unconfigured_default)
    };
    let Some(entry) = rows.iter().find(|entry| entry.id == wanted).cloned() else {
        tracing::warn!(
            provider_id = wanted,
            "a binding names a provider that is not on file; answering from the default one"
        );
        let entry = fallback();
        return Binding {
            provider_id: entry.id,
            settings: entry.settings,
        };
    };

    let model = session_model
        .or_else(|| {
            (session_provider.is_none() && !request.identity_model.is_empty())
                .then_some(request.identity_model)
        })
        .unwrap_or(&entry.settings.model)
        .to_owned();

    let mut settings = entry.settings;
    if model != settings.model {
        settings.model = model;
        settings.max_output_tokens = None;
    }
    Binding {
        provider_id: entry.id,
        settings,
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Accepts a base URL, or says exactly what is wrong with it.
///
/// Trimmed, without a trailing slash; empty means unset. Refuses unparseable
/// URLs, non-HTTP schemes, and a base that includes the endpoint path.
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
/// Only shape is checked; the server says whether the model exists.
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

/// Accepts a row label, or says what is wrong with it. Empty is allowed.
pub fn normalize_label(raw: &str) -> AppResult<String> {
    let trimmed = raw.trim();

    if trimmed.chars().count() > LABEL_MAX_CHARS {
        return Err(AppError::Settings {
            field: "label",
            reason: format!("keep it under {LABEL_MAX_CHARS} characters"),
        });
    }
    Ok(trimmed.to_owned())
}

/// What a save or an add carries for one row, before normalizing.
#[derive(Debug, Clone, Copy)]
pub struct RowDraft<'a> {
    /// `None` keeps the stored label.
    pub label: Option<&'a str>,
    /// The base URL as typed.
    pub base_url: &'a str,
    /// The model id as typed.
    pub model: &'a str,
    /// How the row authenticates.
    pub auth_kind: AuthKind,
    /// The model's output ceiling, looked up by the caller.
    pub max_output_tokens: Option<u32>,
}

impl RowDraft<'_> {
    /// The settings this draft stands for, validated.
    fn settings(&self) -> AppResult<ProviderSettings> {
        Ok(ProviderSettings {
            auth_kind: self.auth_kind,
            base_url: normalize_base_url(self.base_url)?,
            model: normalize_model(self.model)?,
            max_output_tokens: self.max_output_tokens,
        })
    }
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// What the store holds in memory: the rows, the decision settings, and the
/// keys it does not own. Every save writes all three (PLAN 7.18, *Trap*).
#[derive(Debug)]
struct Document {
    providers: Vec<ProviderEntry>,
    decision: DecisionSettings,
    rest: Map<String, Value>,
}

impl Document {
    fn empty() -> Self {
        Self {
            providers: vec![ProviderEntry::unconfigured_default()],
            decision: DecisionSettings::default(),
            rest: Map::new(),
        }
    }

    /// The rows of a file as read: `providers` when present, else the
    /// singleton `provider`; in every case exactly one default row, first.
    fn from_file(file: SettingsFile) -> Self {
        let listed = if file.providers.is_empty() {
            vec![ProviderEntry {
                settings: file.provider.unwrap_or_default(),
                ..ProviderEntry::unconfigured_default()
            }]
        } else {
            file.providers
        };

        let mut providers: Vec<ProviderEntry> = Vec::with_capacity(listed.len());
        for entry in listed {
            if entry.id.trim().is_empty() || providers.iter().any(|kept| kept.id == entry.id) {
                tracing::warn!(id = %entry.id, "a provider row with a blank or repeated id was dropped");
                continue;
            }
            providers.push(entry);
        }
        match providers.iter().position(ProviderEntry::is_default) {
            Some(0) => {}
            Some(at) => {
                let default = providers.remove(at);
                providers.insert(0, default);
            }
            None => providers.insert(0, ProviderEntry::unconfigured_default()),
        }

        Self {
            providers,
            decision: file.decision,
            rest: file.rest,
        }
    }
}

/// The settings store: the rows in memory plus the document backing them.
///
/// Written out whole on every change, under the one lock.
#[derive(Debug)]
pub struct SettingsStore {
    path: PathBuf,
    document: Mutex<Document>,
}

impl SettingsStore {
    /// Loads the settings from `data_dir`.
    ///
    /// Never fails: unreadable settings start with one unconfigured row.
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(SETTINGS_FILE);

        let document = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<SettingsFile>(strip_bom(&bytes)) {
                Ok(file) if file.version == SCHEMA_VERSION => {
                    let document = Document::from_file(file);
                    tracing::info!(
                        providers = document.providers.len(),
                        configured = document.providers[0].settings.is_configured(),
                        "settings loaded"
                    );
                    document
                }
                Ok(file) => {
                    tracing::error!(
                        found = file.version,
                        expected = SCHEMA_VERSION,
                        "unknown settings version"
                    );
                    quarantine(&path);
                    Document::empty()
                }
                Err(err) => {
                    tracing::error!(%err, "settings are not readable JSON");
                    quarantine(&path);
                    Document::empty()
                }
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                tracing::info!("no settings yet; no provider is configured");
                Document::empty()
            }
            Err(err) => {
                tracing::error!(%err, "could not read the settings");
                Document::empty()
            }
        };

        Self {
            path,
            document: Mutex::new(document),
        }
    }

    /// Locks the document, recovering from poison: it cannot be left torn.
    fn document(&self) -> MutexGuard<'_, Document> {
        self.document
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Every row, the default one first.
    pub fn list(&self) -> Vec<ProviderEntry> {
        self.document().providers.clone()
    }

    /// One row by id.
    pub fn entry(&self, id: &str) -> Option<ProviderEntry> {
        self.document()
            .providers
            .iter()
            .find(|entry| entry.id == id)
            .cloned()
    }

    /// Whether a row carries `id`.
    pub fn contains(&self, id: &str) -> bool {
        self.document().providers.iter().any(|entry| entry.id == id)
    }

    /// The default row's settings.
    pub fn get(&self) -> ProviderSettings {
        self.document().providers[0].settings.clone()
    }

    /// Replaces the default row's settings, keeping its label.
    pub fn set(
        &self,
        base_url: &str,
        model: &str,
        auth_kind: AuthKind,
        max_output_tokens: Option<u32>,
    ) -> AppResult<ProviderSettings> {
        let draft = RowDraft {
            label: None,
            base_url,
            model,
            auth_kind,
            max_output_tokens,
        };
        self.update(DEFAULT_PROVIDER_ID, &draft)
            .map(|entry| entry.settings)
    }

    /// Replaces one row's settings. Validated first, so a rejection changes
    /// nothing; an unknown id is refused.
    pub fn update(&self, id: &str, draft: &RowDraft<'_>) -> AppResult<ProviderEntry> {
        let settings = draft.settings()?;
        let label = draft.label.map(normalize_label).transpose()?;

        let mut document = self.document();
        let mut next = document.providers.clone();
        let entry = next
            .iter_mut()
            .find(|entry| entry.id == id)
            .ok_or_else(|| AppError::ProviderNotFound { id: id.to_owned() })?;
        entry.settings = settings;
        if let Some(label) = label {
            entry.label = label;
        }
        let updated = entry.clone();

        self.save(&next, &document)?;
        document.providers = next;
        Ok(updated)
    }

    /// Appends a row under a fresh UUID. Refused past [`PROVIDERS_MAX`].
    pub fn add(&self, draft: &RowDraft<'_>) -> AppResult<ProviderEntry> {
        let settings = draft.settings()?;
        let label = normalize_label(draft.label.unwrap_or_default())?;

        let mut document = self.document();
        if document.providers.len() >= PROVIDERS_MAX {
            return Err(AppError::Settings {
                field: "provider",
                reason: format!("at most {PROVIDERS_MAX} providers on one machine"),
            });
        }

        let entry = ProviderEntry {
            id: Uuid::new_v4().to_string(),
            label,
            settings,
        };
        let mut next = document.providers.clone();
        next.push(entry.clone());

        self.save(&next, &document)?;
        document.providers = next;
        tracing::info!(id = %entry.id, "provider row added");
        Ok(entry)
    }

    /// Removes a row. Refused for [`DEFAULT_PROVIDER_ID`]; whether anything
    /// still names the row is the caller's check
    /// ([`AppState::delete_provider`](crate::AppState::delete_provider)).
    pub fn delete(&self, id: &str) -> AppResult<()> {
        if id == DEFAULT_PROVIDER_ID {
            return Err(AppError::Settings {
                field: "provider",
                reason: "the default provider cannot be deleted — the built-in Assistant \
                         answers from it"
                    .to_owned(),
            });
        }

        let mut document = self.document();
        let mut next = document.providers.clone();
        let before = next.len();
        next.retain(|entry| entry.id != id);
        if next.len() == before {
            return Err(AppError::ProviderNotFound { id: id.to_owned() });
        }

        self.save(&next, &document)?;
        document.providers = next;
        tracing::info!(id, "provider row deleted");
        Ok(())
    }

    /// Writes the document: the rows given, and every key this build does not
    /// own as it was read.
    fn save(&self, providers: &[ProviderEntry], document: &Document) -> AppResult<()> {
        self.write(providers, &document.decision, &document.rest)
    }

    /// Writes one whole document: rows, decision settings, and every key this
    /// build does not own as it was read.
    fn write(
        &self,
        providers: &[ProviderEntry],
        decision: &DecisionSettings,
        rest: &Map<String, Value>,
    ) -> AppResult<()> {
        let file = SettingsFile {
            version: SCHEMA_VERSION,
            providers: providers.to_vec(),
            provider: None,
            decision: decision.clone(),
            rest: rest.clone(),
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

    /// The decision client's settings (PLAN 7.18).
    pub fn decision(&self) -> DecisionSettings {
        self.document().decision.clone()
    }

    /// Replaces the decision settings, validated first. The rows are written
    /// back as they are.
    pub fn set_decision(
        &self,
        model: &str,
        base_url: &str,
        annotate_approvals: bool,
    ) -> AppResult<DecisionSettings> {
        let next = DecisionSettings {
            model: normalize_model(model)?,
            base_url: normalize_decision_base_url(base_url)?,
            annotate_approvals,
        };

        let mut document = self.document();
        self.write(&document.providers, &next, &document.rest)?;
        document.decision = next.clone();
        Ok(next)
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
            .set(
                "https://api.openai.com/v1",
                "gpt-4o-mini",
                AuthKind::ApiKey,
                None,
            )
            .expect("accepted");

        let written = fs::read_to_string(store.path()).expect("the document");
        assert!(written.contains("api.openai.com"));
        assert!(written.contains("gpt-4o-mini"));
        assert!(
            !written.contains("sk-") && !written.contains("oat01"),
            "the settings document contains a secret: {written}"
        );
    }

    #[test]
    fn settings_survive_a_restart() {
        let dir = TempDir::new().expect("temp dir");

        SettingsStore::load(dir.path())
            .set(
                "https://example.test/v1",
                "some-model",
                AuthKind::ApiKey,
                None,
            )
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
            .set(
                "https://example.test/v1",
                "some-model",
                AuthKind::ApiKey,
                None,
            )
            .expect("accepted");
        store
            .set("not a url", "some-model", AuthKind::ApiKey, None)
            .expect_err("refused");

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

    #[test]
    fn each_cli_kind_has_a_https_default_url() {
        for kind in [AuthKind::ClaudeCli, AuthKind::CodexCli, AuthKind::GrokCli] {
            let url = kind.default_base_url();
            assert!(url.starts_with("https://"), "{kind:?} defaulted to {url}");
            assert!(!kind.default_model().is_empty(), "{kind:?} has no model");
        }
        assert_eq!(AuthKind::presets().len(), 5);
        assert_eq!(
            AuthKind::Gemini.default_base_url(),
            "https://generativelanguage.googleapis.com/v1beta"
        );
        assert_eq!(AuthKind::Gemini.default_model(), "gemini-2.5-flash");
        assert!(!AuthKind::Gemini.is_cli());
    }

    #[test]
    fn a_cli_login_is_configured_with_a_model_alone() {
        let provider = ProviderSettings {
            auth_kind: AuthKind::ClaudeCli,
            base_url: String::new(),
            model: "claude-sonnet-4-6".to_owned(),
            max_output_tokens: None,
        };
        assert!(provider.is_configured());
    }

    #[test]
    fn gemini_is_configured_with_a_model_alone() {
        let provider = ProviderSettings {
            auth_kind: AuthKind::Gemini,
            base_url: String::new(),
            model: "gemini-2.5-flash".to_owned(),
            max_output_tokens: None,
        };
        assert!(provider.is_configured());
        assert!(!provider.auth_kind.is_cli());
    }

    #[test]
    fn an_old_document_without_auth_kind_is_an_api_key() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(SETTINGS_FILE);
        fs::write(
            &path,
            br#"{"version":1,"provider":{"base_url":"https://x.test/v1","model":"m"}}"#,
        )
        .expect("write");

        let loaded = SettingsStore::load(dir.path()).get();
        assert_eq!(loaded.auth_kind, AuthKind::ApiKey);
        assert_eq!(loaded.base_url, "https://x.test/v1");
        assert!(loaded.is_configured());
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

    fn entry(id: &str, model: &str, cap: Option<u32>) -> ProviderEntry {
        ProviderEntry {
            id: id.to_owned(),
            label: String::new(),
            settings: ProviderSettings {
                auth_kind: AuthKind::ApiKey,
                base_url: format!("https://{id}.test/v1"),
                model: model.to_owned(),
                max_output_tokens: cap,
            },
        }
    }

    /// The resolution order PLAN 7.19 fixes, one row per case.
    #[test]
    fn a_binding_resolves_session_then_identity_then_row() {
        let rows = [
            entry(DEFAULT_PROVIDER_ID, "d-model", Some(100)),
            entry("second", "s-model", Some(200)),
        ];
        let identity = |provider: &'static str, model: &'static str| BindingRequest {
            identity_provider: provider,
            identity_model: model,
            ..BindingRequest::default()
        };

        let cases: [(&str, BindingRequest<'_>, &str, &str, Option<u32>); 8] = [
            (
                "identity default",
                identity(DEFAULT_PROVIDER_ID, ""),
                DEFAULT_PROVIDER_ID,
                "d-model",
                Some(100),
            ),
            (
                "identity row, row model",
                identity("second", ""),
                "second",
                "s-model",
                Some(200),
            ),
            (
                "identity model",
                identity("second", "s-big"),
                "second",
                "s-big",
                None,
            ),
            (
                "session model only",
                BindingRequest {
                    session_model: Some("s-small"),
                    ..identity("second", "s-big")
                },
                "second",
                "s-small",
                None,
            ),
            (
                "session provider only",
                BindingRequest {
                    session_provider: Some(DEFAULT_PROVIDER_ID),
                    ..identity("second", "s-big")
                },
                DEFAULT_PROVIDER_ID,
                "d-model",
                Some(100),
            ),
            (
                "session pair",
                BindingRequest {
                    session_provider: Some(DEFAULT_PROVIDER_ID),
                    session_model: Some("d-other"),
                    ..identity("second", "s-big")
                },
                DEFAULT_PROVIDER_ID,
                "d-other",
                None,
            ),
            (
                "missing row",
                identity("gone", "g-model"),
                DEFAULT_PROVIDER_ID,
                "d-model",
                Some(100),
            ),
            (
                "session model equal to the row's keeps the ceiling",
                BindingRequest {
                    session_model: Some("s-model"),
                    ..identity("second", "s-big")
                },
                "second",
                "s-model",
                Some(200),
            ),
        ];

        for (name, request, provider, model, cap) in cases {
            let binding = resolve(&rows, &request);
            assert_eq!(binding.provider_id, provider, "{name}");
            assert_eq!(binding.settings.model, model, "{name}");
            assert_eq!(binding.settings.max_output_tokens, cap, "{name}");
            assert_eq!(
                binding.settings.base_url,
                format!("https://{provider}.test/v1"),
                "{name}"
            );
        }
    }

    fn row<'a>(label: &'a str, model: &'a str) -> RowDraft<'a> {
        RowDraft {
            label: Some(label),
            base_url: "http://127.0.0.1:11434/v1",
            model,
            auth_kind: AuthKind::ApiKey,
            max_output_tokens: None,
        }
    }

    /// A document from before PLAN 7.19 loads as the default row, and the next
    /// save writes the roster shape and nothing else.
    #[test]
    fn a_singleton_document_becomes_the_default_row() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(SETTINGS_FILE);
        fs::write(
            &path,
            br#"{"version":1,"provider":{"auth_kind":"claude_cli","base_url":"","model":"claude-sonnet-4-6"}}"#,
        )
        .expect("write");

        let store = SettingsStore::load(dir.path());
        let rows = store.list();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, DEFAULT_PROVIDER_ID);
        assert_eq!(rows[0].settings.auth_kind, AuthKind::ClaudeCli);
        assert_eq!(rows[0].settings.model, "claude-sonnet-4-6");

        store.add(&row("Local", "llama3")).expect("added");

        let written: Value =
            serde_json::from_slice(&fs::read(&path).expect("the document")).expect("json");
        assert!(written.get("provider").is_none(), "{written}");
        assert_eq!(written["version"], 1);
        let providers = written["providers"].as_array().expect("a list");
        assert_eq!(providers.len(), 2);
        assert_eq!(providers[0]["id"], DEFAULT_PROVIDER_ID);
        assert_eq!(providers[0]["model"], "claude-sonnet-4-6");
        assert_eq!(providers[1]["label"], "Local");
    }

    /// The trap PLAN 7.19 names: a save of one row must not drop the others,
    /// nor a nested object a later build owns.
    #[test]
    fn a_save_round_trips_every_row_and_every_unknown_key() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(SETTINGS_FILE);
        fs::write(
            &path,
            br#"{"version":1,"providers":[{"id":"default","model":"m"}],"decision":{"model":"jev"}}"#,
        )
        .expect("write");

        let store = SettingsStore::load(dir.path());
        let local = store.add(&row("Local", "llama3")).expect("added");
        store
            .set("https://x.test/v1", "m2", AuthKind::ApiKey, None)
            .expect("saved");

        let written: Value =
            serde_json::from_slice(&fs::read(&path).expect("the document")).expect("json");
        assert_eq!(written["decision"]["model"], "jev", "{written}");

        let reopened = SettingsStore::load(dir.path());
        let ids: Vec<String> = reopened.list().into_iter().map(|entry| entry.id).collect();
        assert_eq!(ids, [DEFAULT_PROVIDER_ID.to_owned(), local.id.clone()]);
        assert_eq!(reopened.get().model, "m2");
        assert_eq!(
            reopened.entry(&local.id).expect("kept").settings.model,
            "llama3"
        );
    }

    #[test]
    fn a_roster_without_a_default_row_gets_one_first() {
        let dir = TempDir::new().expect("temp dir");
        fs::write(
            dir.path().join(SETTINGS_FILE),
            br#"{"version":1,"providers":[{"id":"x","model":"a"},{"id":"x","model":"b"},{"id":"default","model":"d"}]}"#,
        )
        .expect("write");

        let rows = SettingsStore::load(dir.path()).list();
        let ids: Vec<&str> = rows.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, [DEFAULT_PROVIDER_ID, "x"]);
        assert_eq!(rows[0].settings.model, "d");
        assert_eq!(rows[1].settings.model, "a", "the first of two repeats wins");
    }

    #[test]
    fn an_added_row_gets_a_minted_id_and_a_checked_label() {
        let (_dir, store) = {
            let dir = TempDir::new().expect("temp dir");
            let store = SettingsStore::load(dir.path());
            (dir, store)
        };

        let added = store.add(&row("  Local  ", "llama3")).expect("added");
        assert!(Uuid::parse_str(&added.id).is_ok(), "{}", added.id);
        assert_eq!(added.label, "Local");
        assert!(store.contains(&added.id));

        let long = "x".repeat(LABEL_MAX_CHARS + 1);
        let err = store.add(&row(&long, "llama3")).expect_err("refused");
        assert_eq!(
            serde_json::to_value(&err).expect("serializes")["field"],
            "label"
        );
        assert_eq!(store.list().len(), 2, "a refusal adds nothing");
    }

    #[test]
    fn an_update_keeps_the_label_unless_one_is_given() {
        let dir = TempDir::new().expect("temp dir");
        let store = SettingsStore::load(dir.path());
        let added = store.add(&row("Local", "llama3")).expect("added");

        let kept = store
            .update(
                &added.id,
                &RowDraft {
                    label: None,
                    ..row("", "qwen3")
                },
            )
            .expect("updated");
        assert_eq!(kept.label, "Local");
        assert_eq!(kept.settings.model, "qwen3");

        let err = store
            .update("no-such-row", &row("x", "m"))
            .expect_err("refused");
        assert!(matches!(err, AppError::ProviderNotFound { .. }), "{err}");
    }

    #[test]
    fn the_roster_is_capped() {
        let dir = TempDir::new().expect("temp dir");
        let store = SettingsStore::load(dir.path());

        for _ in 1..PROVIDERS_MAX {
            store.add(&row("", "m")).expect("under the cap");
        }
        let err = store.add(&row("", "m")).expect_err("over the cap");
        assert!(err.to_string().contains("16"), "{err}");
        assert_eq!(store.list().len(), PROVIDERS_MAX);
    }

    #[test]
    fn the_default_row_cannot_be_deleted_and_others_can() {
        let dir = TempDir::new().expect("temp dir");
        let store = SettingsStore::load(dir.path());
        let added = store.add(&row("Local", "llama3")).expect("added");

        let err = store.delete(DEFAULT_PROVIDER_ID).expect_err("refused");
        assert_eq!(
            serde_json::to_value(&err).expect("serializes")["code"],
            "E_INVALID_SETTING"
        );

        store.delete(&added.id).expect("deleted");
        assert!(!store.contains(&added.id));
        assert!(matches!(
            store.delete(&added.id),
            Err(AppError::ProviderNotFound { .. })
        ));
        assert_eq!(SettingsStore::load(dir.path()).list().len(), 1);
    }

    /// PLAN 7.18: a document from before the decision half loads with the
    /// toggle on and both strings empty, resolved at use time.
    #[test]
    fn a_document_without_a_decision_key_loads_with_defaults() {
        let dir = TempDir::new().expect("temp dir");
        fs::write(
            dir.path().join(SETTINGS_FILE),
            br#"{"version":1,"providers":[{"id":"default","model":"m"}]}"#,
        )
        .expect("write");

        let decision = SettingsStore::load(dir.path()).decision();
        assert_eq!(decision, DecisionSettings::default());
        assert!(decision.annotate_approvals);
        assert_eq!(decision.resolved_model(), "jev-latest");
        assert_eq!(decision.resolved_base_url(), "https://api.typesafe.ai");
    }

    /// The trap: a decision save keeps every row, and a row save keeps the
    /// decision settings.
    #[test]
    fn both_halves_survive_either_save() {
        let dir = TempDir::new().expect("temp dir");
        let store = SettingsStore::load(dir.path());
        let local = store.add(&row("Local", "llama3")).expect("added");

        store
            .set_decision("jev-2", "http://127.0.0.1:9/", false)
            .expect("saved");
        store
            .set("https://x.test/v1", "m2", AuthKind::ApiKey, None)
            .expect("saved");

        let reopened = SettingsStore::load(dir.path());
        assert!(reopened.contains(&local.id));
        assert_eq!(
            reopened.decision(),
            DecisionSettings {
                model: "jev-2".to_owned(),
                base_url: "http://127.0.0.1:9".to_owned(),
                annotate_approvals: false,
            }
        );
        let written = fs::read_to_string(store.path()).expect("the document");
        assert!(!written.contains("\"provider\""), "{written}");
    }

    #[test]
    fn a_decision_url_that_names_the_endpoint_is_refused() {
        let dir = TempDir::new().expect("temp dir");
        let store = SettingsStore::load(dir.path());

        let err = store
            .set_decision("", "https://api.typesafe.ai/v1/systemone", true)
            .expect_err("refused");
        assert!(err.to_string().contains("appends that itself"), "{err}");
        assert!(normalize_decision_base_url("typesafe.ai")
            .expect_err("no scheme")
            .to_string()
            .contains("api.typesafe.ai"));
        assert_eq!(store.decision(), DecisionSettings::default());
    }
}
