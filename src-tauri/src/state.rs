//! Application-wide runtime state, managed by Tauri.
//!
//! Each concern owns its own lock, and no command holds two. This is also where
//! facts that span stores are composed — a [`SessionSummary`] needs both the
//! session document and the turn registry.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::agent::decision::{self, DecisionClient};
use crate::agent::provider::{catalog, motosan, openai};
use crate::agent::{
    FakeProvider, ModelCatalog, OpenAiProvider, Provider, ProviderProbe, SubscriptionProvider,
    TurnRegistry,
};
use crate::approval::{ApprovalRegistry, ApprovalRequest, Decision, Resolution};
use crate::audit::{AuditEntry, AuditLog};
use crate::board::trace::RunTrace;
use crate::board::{self, trace};
use crate::error::{AppError, AppResult};
use crate::exec_host::ExecHost;
use crate::mcp::{ConnectorView, Connectors};
use crate::oauth;
use crate::policy::GrantStore;
use crate::schedule::runner::Scheduler;
use crate::secrets::{self, key_hint, ApiKey, KeySource, SecretStore};
use crate::store::{
    self, Agent, AgentDraft, AgentStore, AuthKind, Binding, BindingRequest, Connector,
    ConnectorStore, MaskedDecision, MaskedProvider, MaskedSettings, Memory, MemoryDraft,
    MemoryStore, ProviderEntry, Routine, RoutineStore, RowDraft, SessionDetail, SessionState,
    SessionStore, SessionSummary, SettingsStore, Store, DEFAULT_AGENT_ID, DEFAULT_PROVIDER_ID,
};

/// How many audit lines a board is folded from (Phase 17): the most
/// [`AuditLog::tail`](crate::audit::AuditLog::tail) allows, so fewer runs are
/// cut in half. The panel says how far back it reaches.
const AUDIT_WINDOW: usize = 1000;

/// Shared state, registered with `Manager::manage` and read from commands via
/// `tauri::State<'_, AppState>`.
#[derive(Debug)]
pub struct AppState {
    started_at: Instant,
    quitting: AtomicBool,
    /// Set only after [`crate::tray::init`] succeeds. Close-to-hide and
    /// "stay resident" are tray behaviours; without an icon they strand the
    /// process with no way back (PLAN 5.3).
    tray: AtomicBool,
    store: Store,
    sessions: SessionStore,
    agents: AgentStore,
    memories: MemoryStore,
    routines: RoutineStore,
    /// Which routines are running now, so nothing fires twice (Phase 16). In
    /// memory only: nothing is running after a crash.
    scheduler: Scheduler,
    settings: SettingsStore,
    /// The connectors somebody configured (Phase 18).
    connector_store: ConnectorStore,
    /// The connectors actually running. Processes, unlike the document above;
    /// [`AppState::connector_views`] joins the two.
    connectors: Connectors,
    secrets: SecretStore,
    turns: TurnRegistry,
    grants: GrantStore,
    approvals: ApprovalRegistry,
    audit: AuditLog,
    /// The connection pool every real request goes through, or `None` when
    /// this machine would not give us one (see
    /// [`openai::client`](crate::agent::provider::openai::client)).
    http: Option<reqwest::Client>,
    self_exe: Option<PathBuf>,
    captures: PathBuf,
    attachments: PathBuf,
    skills: PathBuf,
    /// The files the OS last dropped on the window, until the window names
    /// them (PLAN 7.15). In memory only: a drop is a gesture, not a record.
    drops: crate::intake::Drops,
}

impl AppState {
    /// Builds the state for a fresh process from `data_dir`. Infallible: an
    /// unreadable store starts empty, and the failure surfaces on the first save.
    pub fn new(data_dir: &Path) -> Self {
        Self {
            started_at: Instant::now(),
            quitting: AtomicBool::new(false),
            tray: AtomicBool::new(false),
            store: Store::load(data_dir),
            sessions: SessionStore::load(data_dir),
            agents: AgentStore::load(data_dir),
            memories: MemoryStore::load(data_dir),
            routines: RoutineStore::load(data_dir),
            scheduler: Scheduler::new(),
            settings: SettingsStore::load(data_dir),
            connector_store: ConnectorStore::load(data_dir),
            // Empty here: connectors start asynchronously
            // (`commands::connector::spawn`), so the window does not wait.
            connectors: Connectors::new(),
            secrets: SecretStore::new(),
            turns: TurnRegistry::new(),
            grants: GrantStore::new(),
            approvals: ApprovalRegistry::new(),
            audit: AuditLog::new(data_dir),
            // Built once. No credential read at startup: it could prompt.
            http: openai::client(),
            // Only used to refuse `shell_exec` on Aegis itself.
            self_exe: std::env::current_exe()
                .inspect_err(|err| {
                    tracing::warn!(%err, "could not resolve this executable's path");
                })
                .ok(),
            // Beside the stores (PLAN 5.4), created now so `lib.rs` can scope
            // the asset protocol to it.
            captures: prepare_captures(data_dir),
            // Same rules as captures (PLAN 7.20): beside the stores, on the
            // `asset:` scope from startup.
            attachments: prepare_dir(data_dir, crate::attach::DIR),
            // Seeded once per name (`skills::seed`); ordinary files after that.
            skills: prepare_skills(data_dir),
            drops: crate::intake::Drops::new(),
        }
    }

    /// The drop held for the window (PLAN 7.15): paths the OS handed over,
    /// which the WebView names only by id.
    pub fn drops(&self) -> &crate::intake::Drops {
        &self.drops
    }

    /// The session store: transcripts and titles.
    pub fn sessions(&self) -> &SessionStore {
        &self.sessions
    }

    /// Which sessions are running, and how to cancel them.
    pub fn turns(&self) -> &TurnRegistry {
        &self.turns
    }

    /// The default row's provider, as a session of the built-in identity with no
    /// override would get it.
    pub fn provider(&self) -> Box<dyn Provider> {
        self.build_provider(&self.binding(&Agent::builtin(), None))
    }

    /// Who answers one turn of `session_id` as `agent` (PLAN 7.19), decided per
    /// turn so a settings, identity or override change applies to the next
    /// message. Turns, handoffs and routines all come through here.
    pub fn provider_for(&self, agent: &Agent, session_id: &str) -> Box<dyn Provider> {
        let binding = self.sessions.binding_of(session_id).unwrap_or_else(|err| {
            tracing::warn!(%err, session_id, "no session to read a binding from");
            (None, None)
        });
        self.build_provider(
            &self.binding(agent, Some((binding.0.as_deref(), binding.1.as_deref()))),
        )
    }

    /// The row and model `agent` answers with, under a session's override.
    pub fn binding(&self, agent: &Agent, session: Option<(Option<&str>, Option<&str>)>) -> Binding {
        let (session_provider, session_model) = session.unwrap_or((None, None));
        store::settings::resolve(
            &self.settings.list(),
            &BindingRequest {
                identity_provider: &agent.provider_id,
                identity_model: &agent.model,
                session_provider,
                session_model,
            },
        )
    }

    /// The provider for a resolved binding.
    ///
    /// An unconfigured row gets the scripted provider. A configured row gets the
    /// real one even with no key, so the user sees `E_NO_API_KEY` rather than a
    /// silent fake
    /// ([`ProviderSettings::is_configured`](crate::store::ProviderSettings::is_configured)).
    fn build_provider(&self, binding: &Binding) -> Box<dyn Provider> {
        let settings = binding.settings.clone();

        if !settings.is_configured() {
            return Box::new(FakeProvider::new());
        }

        let key = self.row_key(&binding.provider_id, settings.auth_kind);

        // motosan handles the CLI logins, Gemini, and an API key for
        // Anthropic's own host, whose compatibility layer drops prompt caching.
        if catalog::uses_motosan(settings.auth_kind, &settings.base_url) {
            return Box::new(SubscriptionProvider::new(settings, key, self.http.clone()));
        }

        Box::new(OpenAiProvider::new(self.http.clone(), &settings, key))
    }

    /// A row's stored key. A CLI login reads its own file, so it has none here.
    fn row_key(&self, provider_id: &str, kind: AuthKind) -> Option<ApiKey> {
        if kind.is_cli() {
            return None;
        }
        self.secrets
            .inspect_account(&secrets::account_for(provider_id))
            .key
    }

    /// The roster and what may be shown about each key: rows from disk, key
    /// facts from one platform read per keyed row
    /// ([`SecretStore::inspect_account`](crate::secrets::SecretStore::inspect_account)).
    pub fn masked_settings(&self) -> MaskedSettings {
        // The default account answers for availability even when its row is a
        // CLI login, which never reads the store.
        let default_held = self.secrets.inspect();
        let keyring_available = default_held.keyring_available;

        let providers = self
            .settings
            .list()
            .into_iter()
            .map(|entry| {
                let kind = entry.settings.auth_kind;
                let (key_source, key_hint) = if kind.is_cli() {
                    let source = match kind {
                        AuthKind::ClaudeCli => KeySource::ClaudeCli,
                        AuthKind::CodexCli => KeySource::CodexCli,
                        AuthKind::GrokCli => KeySource::GrokCli,
                        AuthKind::ApiKey | AuthKind::Gemini => KeySource::None,
                    };
                    let hint = oauth::peek(kind).map(|peek| key_hint(peek.access_token.expose()));
                    (source, hint)
                } else {
                    let held = if entry.is_default() {
                        None
                    } else {
                        Some(
                            self.secrets
                                .inspect_account(&secrets::account_for(&entry.id)),
                        )
                    };
                    let held = held.as_ref().unwrap_or(&default_held);
                    (
                        held.source,
                        held.key.as_ref().map(|key| key_hint(key.expose())),
                    )
                };

                MaskedProvider {
                    id: entry.id,
                    label: entry.label,
                    auth_kind: kind,
                    base_url: entry.settings.base_url,
                    model: entry.settings.model,
                    max_output_tokens: entry.settings.max_output_tokens,
                    key_source,
                    key_hint,
                }
            })
            .collect();

        MaskedSettings {
            providers,
            keyring_available,
            presets: AuthKind::presets().to_vec(),
            decision: self.masked_decision(),
        }
    }

    // -----------------------------------------------------------------------
    // Decision model (PLAN 7.18)
    // -----------------------------------------------------------------------

    /// What the WebView may know about the decision client. One more platform
    /// read, for `typesafe-api-key`.
    fn masked_decision(&self) -> MaskedDecision {
        let held = self.secrets.inspect_account(secrets::TYPESAFE_ACCOUNT);
        let settings = self.settings.decision();
        MaskedDecision {
            key_source: held.source,
            key_hint: held.key.as_ref().map(|key| key_hint(key.expose())),
            model: settings.model,
            base_url: settings.base_url,
            annotate_approvals: settings.annotate_approvals,
            default_model: store::settings::DECISION_DEFAULT_MODEL.to_owned(),
            default_base_url: store::settings::DECISION_DEFAULT_BASE_URL.to_owned(),
        }
    }

    /// The decision client as Settings stand now, or `None` without a key or
    /// an HTTP client. Built per turn, like the provider, and never from the
    /// chat key.
    pub fn decision_client(&self) -> Option<DecisionClient> {
        self.try_decision_client()
            .inspect_err(|err| {
                if *err != decision::DecisionError::NoKey {
                    tracing::warn!(%err, "no decision client");
                }
            })
            .ok()
    }

    fn try_decision_client(&self) -> Result<DecisionClient, decision::DecisionError> {
        let key = self.secrets.inspect_account(secrets::TYPESAFE_ACCOUNT).key;
        DecisionClient::new(self.http.clone(), key, &self.settings.decision())
    }

    /// Saves the decision settings, then the key when one is given. A blank
    /// key keeps the stored one.
    pub fn save_decision(
        &self,
        model: &str,
        base_url: &str,
        annotate_approvals: bool,
        api_key: Option<&str>,
    ) -> AppResult<()> {
        self.settings
            .set_decision(model, base_url, annotate_approvals)?;
        match api_key.and_then(ApiKey::new) {
            Some(key) => self.secrets.store_account(secrets::TYPESAFE_ACCOUNT, &key),
            None => Ok(()),
        }
    }

    /// Removes the stored TypeSafe key. `AEGIS_TYPESAFE_API_KEY` is untouched.
    pub fn clear_decision_key(&self) -> AppResult<()> {
        self.secrets.clear_account(secrets::TYPESAFE_ACCOUNT)
    }

    /// One cheap question to TypeSafe, reported like a chat probe.
    pub async fn probe_decision(&self) -> ProviderProbe {
        match self.try_decision_client() {
            Ok(client) => client.probe().await,
            Err(err) => decision::unusable_probe(&err),
        }
    }

    /// The row `provider_id` names, or `E_INVALID_SETTING` on `field`.
    fn known_row(&self, provider_id: &str, field: &'static str) -> AppResult<ProviderEntry> {
        self.settings
            .entry(provider_id)
            .ok_or_else(|| AppError::Settings {
                field,
                reason: format!("`{provider_id}` is not a provider in Settings"),
            })
    }

    /// Saves one row, and its key when one is given.
    ///
    /// Settings are validated and written before the key, so a rejected URL
    /// never touches the credential store. The model's output ceiling is
    /// looked up first, with the key about to be stored.
    pub async fn save_provider(
        &self,
        provider_id: &str,
        draft: &RowDraft<'_>,
        api_key: Option<&str>,
    ) -> AppResult<ProviderEntry> {
        if !self.settings.contains(provider_id) {
            return Err(AppError::ProviderNotFound {
                id: provider_id.to_owned(),
            });
        }
        let cap = self
            .model_output_cap(
                provider_id,
                draft.auth_kind,
                draft.base_url,
                draft.model,
                api_key,
            )
            .await;

        let saved = self.settings.update(
            provider_id,
            &RowDraft {
                max_output_tokens: cap,
                ..*draft
            },
        )?;
        self.store_row_key(&saved, api_key)?;
        Ok(saved)
    }

    /// Appends a row under a fresh id, and its key when one is given.
    pub async fn add_provider(
        &self,
        draft: &RowDraft<'_>,
        api_key: Option<&str>,
    ) -> AppResult<ProviderEntry> {
        let pending = api_key.and_then(ApiKey::new);
        let cap = catalog::output_cap(
            draft.auth_kind,
            draft.base_url,
            draft.model,
            self.http.as_ref(),
            pending.as_ref(),
        )
        .await;

        let added = self.settings.add(&RowDraft {
            max_output_tokens: cap,
            ..*draft
        })?;
        self.store_row_key(&added, api_key)?;
        Ok(added)
    }

    /// Files a row's key. A blank key keeps what is stored; a CLI row stores
    /// none.
    fn store_row_key(&self, entry: &ProviderEntry, api_key: Option<&str>) -> AppResult<()> {
        let Some(key) = api_key.and_then(ApiKey::new) else {
            return Ok(());
        };
        if entry.settings.auth_kind.is_cli() {
            tracing::warn!(id = %entry.id, "a key sent for a CLI login was not stored");
            return Ok(());
        }
        self.secrets
            .store_account(&secrets::account_for(&entry.id), &key)
    }

    /// Removes a row's stored key. Refused for a CLI row: that login is the
    /// CLI's, not Aegis's.
    pub fn clear_provider_key(&self, provider_id: &str) -> AppResult<()> {
        let entry = self
            .settings
            .entry(provider_id)
            .ok_or_else(|| AppError::ProviderNotFound {
                id: provider_id.to_owned(),
            })?;
        if entry.settings.auth_kind.is_cli() {
            return Err(AppError::Settings {
                field: "key",
                reason: "this provider signs in through its CLI; sign out there — Aegis \
                         stores no key for it"
                    .to_owned(),
            });
        }
        self.secrets
            .clear_account(&secrets::account_for(provider_id))
    }

    /// Deletes a row, unless it is the default one or an identity or a
    /// session override still names it. Its key goes with it.
    pub fn delete_provider(&self, provider_id: &str) -> AppResult<()> {
        if provider_id != DEFAULT_PROVIDER_ID {
            let identities = self.agents.count_for_provider(provider_id);
            let sessions = self.sessions.count_for_provider(provider_id);
            if identities > 0 || sessions > 0 {
                return Err(AppError::ProviderInUse {
                    identities,
                    sessions,
                });
            }
        }

        let entry = self.settings.entry(provider_id);
        self.settings.delete(provider_id)?;

        if let Some(entry) = entry.filter(|entry| !entry.settings.auth_kind.is_cli()) {
            if let Err(err) = self.secrets.clear_account(&secrets::account_for(&entry.id)) {
                tracing::warn!(%err, id = %entry.id, "the provider is gone; its key is not");
            }
        }
        Ok(())
    }

    /// Asks one row's server whether it is reachable and the key works.
    pub async fn probe_provider(&self, provider_id: &str) -> AppResult<ProviderProbe> {
        let entry = self
            .settings
            .entry(provider_id)
            .ok_or_else(|| AppError::ProviderNotFound {
                id: provider_id.to_owned(),
            })?;
        let settings = entry.settings;
        let key = self.row_key(&entry.id, settings.auth_kind);

        // The same fork as `build_provider`: the probe must reach what a turn
        // would.
        if catalog::uses_motosan(settings.auth_kind, &settings.base_url) {
            return Ok(motosan::probe(
                settings.auth_kind,
                &settings.model,
                &settings.base_url,
                key,
                self.http.as_ref(),
            )
            .await);
        }

        Ok(openai::probe(self.http.as_ref(), &settings, key.as_ref()).await)
    }

    /// The named model's largest reply, from the provider's catalog.
    /// `pending_key`, from a form not saved yet, wins over the row's stored key.
    pub async fn model_output_cap(
        &self,
        provider_id: &str,
        kind: AuthKind,
        base_url: &str,
        model: &str,
        pending_key: Option<&str>,
    ) -> Option<u32> {
        let key = pending_key
            .and_then(ApiKey::new)
            .or_else(|| self.row_key(provider_id, kind));

        catalog::output_cap(kind, base_url, model, self.http.as_ref(), key.as_ref()).await
    }

    /// The models an authentication kind accepts, for the base URL in the form,
    /// asked with `provider_id`'s stored key.
    pub async fn list_models(
        &self,
        provider_id: &str,
        kind: AuthKind,
        base_url: &str,
    ) -> ModelCatalog {
        catalog::list(
            kind,
            base_url,
            self.http.as_ref(),
            self.row_key(provider_id, kind).as_ref(),
        )
        .await
    }

    /// The provider settings on disk.
    pub fn settings(&self) -> &SettingsStore {
        &self.settings
    }

    /// The API keys, wherever this machine keeps them — never in a document
    /// Aegis writes.
    pub fn secrets(&self) -> &SecretStore {
        &self.secrets
    }

    /// The identities on disk, and the built-in one that is not.
    pub fn agents(&self) -> &AgentStore {
        &self.agents
    }

    /// Creates an identity bound to any row on file.
    pub fn create_agent(&self, draft: &AgentDraft) -> AppResult<Agent> {
        self.agents
            .create_with(draft, &|id| self.settings.contains(id))
    }

    /// Replaces an identity's fields, bound to any row on file.
    pub fn update_agent(&self, agent_id: &str, draft: &AgentDraft) -> AppResult<Agent> {
        self.agents
            .update_with(agent_id, draft, &|id| self.settings.contains(id))
    }

    /// Every identity, for the picker.
    pub fn agent_list(&self) -> Vec<Agent> {
        self.agents.list()
    }

    /// The identity a session runs as. Infallible
    /// ([`AgentStore::resolve`](crate::store::AgentStore::resolve)): a missing
    /// session resolves to the built-in identity, and the turn then fails on the
    /// transcript instead.
    pub fn agent_of(&self, session_id: &str) -> Agent {
        let named = self.sessions.agent_of(session_id).unwrap_or_else(|err| {
            tracing::warn!(%err, session_id, "no session to resolve an identity for");
            None
        });

        self.agents.resolve(named.as_deref())
    }

    /// Creates a session bound to an identity, checked here because the session
    /// store cannot see the agent document or the roster. An omitted
    /// `provider_id` or `model` inherits (PLAN 7.19).
    pub fn create_session(
        &self,
        project_id: &str,
        title: Option<&str>,
        agent_id: Option<&str>,
        provider_id: Option<&str>,
        model: Option<&str>,
    ) -> AppResult<SessionSummary> {
        let agent_id = agent_id.unwrap_or(DEFAULT_AGENT_ID);
        let agent = self.agents.get(agent_id)?;
        let (provider_id, model) = self.check_override(provider_id, model)?;

        self.sessions.create_bound(
            project_id,
            title,
            &agent.id,
            provider_id.as_deref(),
            model.as_deref(),
        )
    }

    /// Writes or clears a session's provider and model override (PLAN 7.19).
    /// Both `None` returns to the identity's pair. Refused while a turn runs;
    /// the next send uses it. The identity is untouched.
    pub fn set_session_binding(
        &self,
        session_id: &str,
        provider_id: Option<&str>,
        model: Option<&str>,
    ) -> AppResult<SessionSummary> {
        if let Some(turn_id) = self.turns.active_turn(session_id) {
            return Err(AppError::TurnBusy { turn_id });
        }
        let (provider_id, model) = self.check_override(provider_id, model)?;

        self.sessions.set_binding(
            session_id,
            provider_id.as_deref(),
            model.as_deref(),
            self.turns.state_of(session_id),
        )
    }

    /// An override as it will be stored: blank is absent, a row must be on
    /// file, a model is one word.
    fn check_override(
        &self,
        provider_id: Option<&str>,
        model: Option<&str>,
    ) -> AppResult<(Option<String>, Option<String>)> {
        let provider_id = provider_id
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(|id| self.known_row(id, "provider").map(|entry| entry.id))
            .transpose()?;
        let model = model
            .map(store::settings::normalize_model)
            .transpose()?
            .filter(|model| !model.is_empty());
        Ok((provider_id, model))
    }

    /// Deletes an identity, unless sessions or routines still use it
    /// ([`AppError::AgentInUse`](crate::AppError::AgentInUse)). Its memories go
    /// with it, since nothing else can reach them; the identity is deleted
    /// first, so a failure leaves unreadable records, not an identity without
    /// its memories.
    pub fn delete_agent(&self, agent_id: &str) -> AppResult<()> {
        let bound = self.sessions.count_for_agent(agent_id);
        if bound > 0 {
            return Err(AppError::AgentInUse { count: bound });
        }
        // Refused: the person deleting knows whether the routine should move or
        // go (Phase 16).
        let fired_by = self.routines.count_for_agent(agent_id);
        if fired_by > 0 {
            return Err(AppError::AgentHasRoutines { count: fired_by });
        }

        self.agents.delete(agent_id)?;

        if let Err(err) = self.memories.forget_for_agent(agent_id) {
            tracing::warn!(%err, agent_id, "the identity is gone; its memories are not");
        }
        Ok(())
    }

    /// Where this installation's memories are kept (PLAN 7.3, Phase 14).
    pub fn memories(&self) -> &MemoryStore {
        &self.memories
    }

    /// The routine store: what is on a clock (PLAN 7.3, Phase 16).
    pub fn routines(&self) -> &RoutineStore {
        &self.routines
    }

    /// The connector document: what somebody configured (PLAN 7.3, Phase 18).
    pub fn connector_store(&self) -> &ConnectorStore {
        &self.connector_store
    }

    /// The connectors that are running, and what they offer.
    pub fn connectors(&self) -> &Connectors {
        &self.connectors
    }

    /// Every connector's row: the configured record joined to its process.
    pub fn connector_views(&self) -> Vec<ConnectorView> {
        self.connectors.views(&self.connector_store.list())
    }

    /// One connector's row.
    pub fn connector_view(&self, connector: &Connector) -> ConnectorView {
        self.connectors.view(connector)
    }

    /// Which routines are running right now.
    pub fn scheduler(&self) -> &Scheduler {
        &self.scheduler
    }

    /// Every routine, with whatever currently stops it from firing.
    pub fn routine_list(&self) -> Vec<Routine> {
        self.routines
            .list()
            .into_iter()
            .map(|mut routine| {
                routine.problem = self.routine_problem(&routine);
                routine
            })
            .collect()
    }

    /// Records where a watched folder stands when its routine is saved, so a
    /// file dropped right after fires on the next tick. Best effort: a missing
    /// folder is learned later ([`schedule::learning`](crate::schedule::learning)).
    pub fn arm_watch(&self, routine: &Routine) {
        let crate::store::Schedule::OnChange { dir } = &routine.schedule else {
            return;
        };
        let Some(workspace) = self.workspace_for_project(&routine.project_id) else {
            return;
        };
        let Ok(resolved) = crate::policy::path::resolve(&workspace, dir) else {
            return;
        };
        if !resolved.inside {
            return;
        }

        if let Some(newest) = crate::schedule::newest_change(&resolved.path) {
            self.routines.mark_seen(&routine.id, &newest);
        }
    }

    /// One routine with its problem measured, as a command hands it back.
    pub fn routine_with_problem(&self, mut routine: Routine) -> Routine {
        routine.problem = self.routine_problem(&routine);
        routine
    }

    /// Why this routine cannot fire, or `None`. The panel and the scheduler read
    /// the same answer.
    pub fn routine_problem(&self, routine: &Routine) -> Option<String> {
        let agent = self.agents.get(&routine.agent_id).ok();
        let workspace = self.workspace_for_project(&routine.project_id);
        let catalog = self.skill_catalog(workspace.as_deref());
        let skill = crate::skills::find(&catalog, &routine.skill).cloned();

        crate::schedule::inspect(
            routine,
            agent.as_ref(),
            workspace.as_deref(),
            skill.as_ref(),
            self.routines.runs_today_for_agent(&routine.agent_id),
        )
    }

    /// The project's board (Phase 17), composed from `STATUS.md`, the session,
    /// turn, approval and routine stores and the audit log — all measured now.
    pub fn board(&self, project_id: &str) -> board::Board {
        let sessions = self.session_list(project_id);
        let routines: Vec<Routine> = self
            .routine_list()
            .into_iter()
            .filter(|routine| routine.project_id == project_id)
            .collect();

        // Only this project's dialogs, by its sessions.
        let approvals: Vec<ApprovalRequest> = self
            .pending_approvals(None)
            .into_iter()
            .filter(|request| {
                sessions
                    .iter()
                    .any(|session| session.id == request.session_id)
            })
            .collect();

        let status = self
            .workspace_for_project(project_id)
            .and_then(|root| crate::workspace::status(&root))
            .map(|(path, text)| (path.display().to_string(), text));

        let runs = trace::fold(&self.audit_window(), &self.ledger(&sessions));

        board::assemble(board::Facts {
            project_id,
            status: status
                .as_ref()
                .map(|(path, text)| (path.as_str(), text.as_str())),
            sessions: &sessions,
            routines: &routines,
            approvals: &approvals,
            runs,
        })
    }

    /// One run and the audit lines it replays from, oldest first (PLAN 7.2,
    /// row 10). Folded again rather than cached.
    pub fn run_trace(&self, project_id: &str, run: &trace::RunRef) -> AppResult<RunTrace> {
        let sessions = self.session_list(project_id);
        let ledger = self.ledger(&sessions);
        let window = self.audit_window();

        let folded = trace::fold(&window, &ledger)
            .into_iter()
            .find(|folded| &folded.run == run)
            .ok_or_else(|| AppError::RunNotFound { id: run.id.clone() })?;

        let known: Vec<&str> = ledger
            .iter()
            .map(|session| session.session_id.as_str())
            .collect();
        let mut entries: Vec<AuditEntry> = window
            .into_iter()
            .filter(|entry| known.contains(&entry.session_id.as_str()))
            .filter(|entry| &trace::RunRef::of(entry) == run)
            .collect();
        // Oldest first: a replay is read forwards, unlike the drawer, which is
        // a tail and is read backwards.
        entries.sort_by(|left, right| left.ts.cmp(&right.ts));

        Ok(RunTrace {
            run: folded,
            entries,
        })
    }

    /// The audit window a board is folded from. A read failure is an empty
    /// window; the drawer reports it.
    fn audit_window(&self) -> Vec<AuditEntry> {
        self.audit.tail(AUDIT_WINDOW, None).unwrap_or_else(|err| {
            tracing::warn!(%err, "the board could not read the audit log");
            Vec::new()
        })
    }

    /// What the fold is allowed to see, and what each session spent.
    fn ledger(&self, sessions: &[SessionSummary]) -> Vec<trace::SessionLedger> {
        sessions
            .iter()
            .map(|session| trace::SessionLedger {
                session_id: session.id.clone(),
                title: session.title.clone(),
                routine: session
                    .scheduled
                    .as_ref()
                    .map(|scheduled| scheduled.routine_name.clone())
                    .unwrap_or_default(),
                handoff: session
                    .delegated
                    .as_ref()
                    .map(|delegated| delegated.handoff_id.clone())
                    .unwrap_or_default(),
                running: matches!(
                    session.state,
                    SessionState::Running | SessionState::AwaitingApproval
                ),
                turns: self.sessions.costs(&session.id).unwrap_or_default(),
            })
            .collect()
    }

    /// A project's workspace folder, when it exists right now.
    pub fn workspace_for_project(&self, project_id: &str) -> Option<PathBuf> {
        self.store
            .list()
            .into_iter()
            .find(|project| project.id == project_id)
            .filter(|project| project.workspace_exists)
            .map(|project| PathBuf::from(project.workspace_path))
    }

    /// Where a project's commands run (PLAN 7.12). `None` for this process, and
    /// for an unknown project, which has no workspace anyway.
    pub fn exec_host_for_project(&self, project_id: &str) -> Option<ExecHost> {
        self.store
            .list()
            .into_iter()
            .find(|project| project.id == project_id)
            .and_then(|project| project.exec_host)
    }

    /// Where a session's commands run: its project's host, never overridden.
    pub fn exec_host_of(&self, session_id: &str) -> Option<ExecHost> {
        let project_id = self.sessions.project_of(session_id).ok()?;
        self.exec_host_for_project(&project_id)
    }

    /// One identity's memories, most recently touched first.
    pub fn memory_list(&self, agent_id: &str) -> AppResult<Vec<Memory>> {
        // A stale picker gets "no such identity", not an empty list.
        let agent = self.agents.get(agent_id)?;
        Ok(self.memories.list_for(&agent.id))
    }

    /// Records or corrects a memory on the user's behalf: correcting memory is
    /// the human's (`COS.md` *Memory*).
    pub fn memory_save(
        &self,
        agent_id: &str,
        memory_id: Option<&str>,
        draft: &MemoryDraft,
    ) -> AppResult<Memory> {
        let agent = self.agents.get(agent_id)?;
        self.memories.save(&agent.id, memory_id, draft)
    }

    /// Forgets one memory.
    pub fn memory_forget(&self, agent_id: &str, memory_id: &str) -> AppResult<Memory> {
        let agent = self.agents.get(agent_id)?;
        self.memories.forget(&agent.id, memory_id)
    }

    /// Folds a session's older turns (Phase 14) and returns the detail, whether
    /// or not anything folded. Refused with `E_TURN_BUSY` while a turn runs, so
    /// a turn's rounds share one context.
    pub fn compact_session(&self, session_id: &str) -> AppResult<SessionDetail> {
        if let Some(turn_id) = self.turns.active_turn(session_id) {
            return Err(AppError::TurnBusy { turn_id });
        }

        self.sessions.compact(session_id, true)?;
        self.session_detail(session_id)
    }

    /// The user's skill library (Phase 13): how this person works, across
    /// projects.
    pub fn skills(&self) -> &Path {
        &self.skills
    }

    /// The runbooks in the library and the workspace, right now.
    pub fn skill_catalog(&self, workspace: Option<&Path>) -> Vec<crate::skills::Skill> {
        crate::skills::catalog(&self.skills, workspace)
    }

    /// This application's own binary, when the platform would name it.
    pub fn self_exe(&self) -> Option<&Path> {
        self.self_exe.as_deref()
    }

    /// Where `screen_capture` writes: the only directory the WebView may read,
    /// through the asset protocol (`lib.rs`), and never a workspace.
    pub fn captures(&self) -> &Path {
        &self.captures
    }

    /// Where attached images are copied (PLAN 7.20): the window reads them
    /// through the asset protocol, like captures, and never a workspace.
    pub fn attachments(&self) -> &Path {
        &self.attachments
    }

    /// A project's sessions, most recently active first, with live states from
    /// the turn registry.
    pub fn session_list(&self, project_id: &str) -> Vec<SessionSummary> {
        self.sessions.list(project_id, &self.turns.lookup())
    }

    /// One session's row, at its live state.
    pub fn session_summary(&self, session_id: &str) -> AppResult<SessionSummary> {
        self.sessions
            .summary(session_id, self.turns.state_of(session_id))
    }

    /// One session with its transcript, live state and pending approvals, so a
    /// window reopened mid-turn redraws a dialog it missed.
    pub fn session_detail(&self, session_id: &str) -> AppResult<SessionDetail> {
        let mut detail = self
            .sessions
            .open(session_id, self.turns.state_of(session_id))?;
        detail.pending_approvals = self.approvals.list(Some(session_id));
        Ok(detail)
    }

    /// Everything a session is blocked on, oldest first.
    pub fn pending_approvals(&self, session_id: Option<&str>) -> Vec<ApprovalRequest> {
        self.approvals.list(session_id)
    }

    /// Answers one approval, recording any grant it creates.
    pub fn resolve_approval(&self, request_id: &str, decision: Decision) -> AppResult<Resolution> {
        self.approvals.resolve(request_id, decision, &self.grants)
    }

    /// Forgets a session, in order: cancel its turn, withdraw its approvals
    /// (releasing a parked turn), then clear its grants — so a running turn
    /// cannot re-create what was cleared.
    pub fn close_session(&self, session_id: &str) {
        self.turns.forget(session_id);
        self.approvals.withdraw_session(session_id);
        self.grants.clear(session_id);
    }

    /// The workspace a session's tools may touch. `Ok(None)` means the folder is
    /// gone, and every call is then refused (PLAN 3.2).
    pub fn workspace_of(&self, session_id: &str) -> AppResult<Option<PathBuf>> {
        let project_id = self.sessions.project_of(session_id)?;

        Ok(self
            .store
            .list()
            .into_iter()
            .find(|project| project.id == project_id)
            .filter(|project| project.workspace_exists)
            .map(|project| PathBuf::from(project.workspace_path)))
    }

    /// The state to leave a session at, given how its turn ended.
    pub fn retire_turn(&self, session_id: &str, turn_id: &str, resting: SessionState) {
        self.turns.finish(session_id, turn_id, resting);
    }

    /// The project store.
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// The live session grants. Never persisted: "allow for this session" must
    /// not become "allow forever" (PLAN 3.1).
    pub fn grants(&self) -> &GrantStore {
        &self.grants
    }

    /// Pending approvals. Not persisted: each is a turn parked on a channel
    /// that does not survive the process.
    pub fn approvals(&self) -> &ApprovalRegistry {
        &self.approvals
    }

    /// The process's single audit log; every line names its session.
    pub fn audit(&self) -> &AuditLog {
        &self.audit
    }

    /// How long this process has been up. Used by logging and, later, by the
    /// diagnostics surface.
    pub fn uptime(&self) -> Duration {
        self.started_at.elapsed()
    }

    /// Whether a real shutdown is in progress: a window close is a hide unless
    /// the user quit.
    pub fn is_quitting(&self) -> bool {
        self.quitting.load(Ordering::SeqCst)
    }

    /// Whether the tray icon is up. Without one, closing the window must end the
    /// process.
    pub fn has_tray(&self) -> bool {
        self.tray.load(Ordering::SeqCst)
    }

    /// Records that the tray icon was installed. Called once from setup.
    pub fn mark_tray(&self) {
        self.tray.store(true, Ordering::SeqCst);
    }

    /// Marks shutdown as started, returning `true` only the first time. Running
    /// turns are cancelled so they answer their open calls
    /// ([`agent::turn`](crate::agent::turn)).
    pub fn begin_quit(&self) -> bool {
        let first = !self.quitting.swap(true, Ordering::SeqCst);
        if first {
            self.turns.cancel_all();
        }
        first
    }
}

/// Creates and seeds the skill library. A directory that cannot be created
/// leaves an empty catalog.
fn prepare_skills(data_dir: &Path) -> PathBuf {
    let library = data_dir.join(crate::skills::LIBRARY_DIR);
    crate::skills::seed(&library);
    library
}

/// Creates the capture directory, canonicalized because Tauri matches the asset
/// scope against canonical paths. A failure surfaces on the first capture, not
/// at startup.
fn prepare_captures(data_dir: &Path) -> PathBuf {
    prepare_dir(data_dir, "captures")
}

/// Creates one of the window-readable directories, canonicalized.
fn prepare_dir(data_dir: &Path, name: &str) -> PathBuf {
    let dir = data_dir.join(name);

    if let Err(err) = std::fs::create_dir_all(&dir) {
        tracing::warn!(%err, dir = %dir.display(), "could not create a data directory");
        return dir;
    }

    dunce::canonicalize(&dir).unwrap_or(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    /// The library is there from startup with the example runbooks in it, so a
    /// fresh install has the format in the place people look for it.
    #[test]
    fn the_skill_library_is_seeded_on_a_first_run() {
        let dir = TempDir::new().expect("temp dir");
        let state = AppState::new(dir.path());

        assert_eq!(state.skills().parent(), Some(dir.path()));

        let catalog = state.skill_catalog(None);
        let names: Vec<&str> = catalog.iter().map(|skill| skill.name.as_str()).collect();
        assert_eq!(
            names,
            [
                crate::skills::ALERT_SKILL,
                crate::skills::BUDGET_ALERT_SKILL,
                crate::skills::BUDGET_POSITION_SKILL,
                crate::skills::BUDGET_RUNWAY_SKILL,
                crate::skills::FOUND_SKILL,
                crate::skills::COS_SKILL,
                crate::skills::DEPLOY_SKILL,
                crate::skills::MAIL_SKILL,
                crate::skills::REVIEW_SKILL,
                crate::skills::REPLY_SKILL,
                crate::skills::REVENUE_PIPELINE_SKILL,
                crate::skills::REVENUE_THESIS_SKILL,
                crate::skills::REVIEW_DIFF_SKILL,
                crate::skills::SOCIAL_POST_SKILL,
                crate::skills::SOCIAL_REPLY_SKILL,
                crate::skills::SOCIAL_SCAN_SKILL,
                crate::skills::THREAD_SKILL,
                crate::skills::WATCH_DIGEST_SKILL,
                crate::skills::WATCH_IMPACT_SKILL,
                crate::skills::WATCH_SWEEP_SKILL,
                crate::skills::WISH_LIST_SKILL,
                crate::skills::CHECK_SKILL,
                crate::skills::DRAFT_SKILL,
                crate::skills::PERCEIVE_SKILL,
                crate::skills::VERIFY_SKILL,
            ]
        );
        for skill in &catalog {
            assert!(skill.runnable(), "{}: {:?}", skill.name, skill.problem);
        }

        // And no identity is offered it until someone grants it: the built-in
        // one is the assistant from before skills existed.
        assert!(crate::skills::granted(&catalog, &Agent::builtin()).is_empty());
    }

    /// Captures go beside the stores and never into a workspace (PLAN 5.4).
    /// The directory exists from startup, because the asset-protocol scope in
    /// `lib.rs` is registered against it before anything has been captured.
    #[test]
    fn the_capture_directory_is_ready_before_anything_is_captured() {
        let dir = TempDir::new().expect("temp dir");
        let state = AppState::new(dir.path());

        assert!(state.captures().is_dir(), "created at startup");
        assert!(
            state
                .captures()
                .starts_with(dunce::canonicalize(dir.path()).expect("canonical data directory")),
            "under the application-data directory"
        );
        assert_eq!(
            state.captures().file_name().and_then(|n| n.to_str()),
            Some("captures")
        );
    }

    #[test]
    fn the_tray_is_absent_until_setup_marks_it() {
        let dir = TempDir::new().expect("temp dir");
        let state = AppState::new(dir.path());
        assert!(
            !state.has_tray(),
            "a missing tray must not trap close-to-hide"
        );
        state.mark_tray();
        assert!(state.has_tray());
    }

    #[test]
    fn quitting_latches_once() {
        let dir = TempDir::new().expect("temp dir");
        let state = AppState::new(dir.path());
        assert!(!state.is_quitting());
        assert!(state.begin_quit(), "first call wins");
        assert!(state.is_quitting());
        assert!(!state.begin_quit(), "second call is a no-op");
        assert!(state.is_quitting());
    }

    /// The audit log the runtime hands out writes into the data directory.
    #[test]
    fn the_audit_log_writes_into_the_data_directory() {
        let dir = TempDir::new().expect("temp dir");
        let state = AppState::new(dir.path());

        let log = state.audit();
        assert_eq!(log.path().parent(), Some(dir.path()));

        log.append(&crate::audit::AuditRecord {
            session_id: "s1",
            agent_id: DEFAULT_AGENT_ID,
            turn_id: "t1",
            call_id: "c1",
            tool: "fs_list",
            skill: "",
            handoff: "",
            routine: "",
            decision: crate::audit::AuditDecision::Auto,
            policy_reason: "a read-only listing inside the workspace",
            args: &serde_json::json!({ "path": "." }),
            outcome: crate::audit::Outcome::Ok,
            duration_ms: 1,
            bytes_in: 0,
            bytes_out: 4,
            error_code: None,
            artifact: None,
        });

        assert_eq!(log.tail(10, None).expect("tail").len(), 1);
        assert!(log.path().is_file(), "the line reached the data directory");
    }

    /// The provider follows the settings: scripted when unconfigured, the real
    /// one once a URL and model are set. Nothing here writes a key.
    ///
    /// Only rows whose kind is a CLI login are added below, so no test reads a
    /// non-default keyring account.
    #[test]
    fn the_provider_follows_the_settings() {
        let dir = TempDir::new().expect("temp dir");
        let state = AppState::new(dir.path());

        assert_eq!(
            state.provider().model(),
            crate::agent::provider::fake::FAKE_MODEL,
            "an unconfigured install still answers"
        );

        state
            .settings()
            .set(
                "https://api.example.test/v1",
                "some-model",
                crate::store::AuthKind::ApiKey,
                None,
            )
            .expect("accepted");

        assert_eq!(
            state.provider().model(),
            "some-model",
            "a configured provider answers as itself, with or without a key"
        );
    }

    /// The panel is shown a source and a hint, never a key — and an
    /// unconfigured install has nothing to hint at.
    #[test]
    fn masked_settings_carry_no_key() {
        let dir = TempDir::new().expect("temp dir");
        let state = AppState::new(dir.path());

        let masked = state.masked_settings();
        assert_eq!(masked.providers.len(), 1);
        assert_eq!(masked.providers[0].id, DEFAULT_PROVIDER_ID);
        assert_eq!(masked.providers[0].base_url, "");
        assert_eq!(masked.providers[0].model, "");
        assert_eq!(
            masked.providers[0].auth_kind,
            crate::store::AuthKind::ApiKey
        );

        let rendered = serde_json::to_string(&masked).expect("serializes");
        assert!(
            !rendered.contains("\"api_key\":"),
            "the payload must not have a key field: {rendered}"
        );

        // PLAN 7.18: the decision half is masked the same way.
        assert!(masked.decision.annotate_approvals);
        assert_eq!(masked.decision.default_model, "jev-latest");
        assert!(!rendered.contains("typesafe-api-key"), "{rendered}");
    }

    /// Grants and the audit log are per-process, not per-store: a second
    /// `AppState` over the same directory must find the lines the first one
    /// wrote, and none of its grants.
    #[test]
    fn a_restart_keeps_the_log_and_drops_the_grants() {
        let dir = TempDir::new().expect("temp dir");

        let first = AppState::new(dir.path());
        first.grants().insert("s1", crate::policy::Grant::FsWrite);
        first.audit().append(&crate::audit::AuditRecord {
            session_id: "s1",
            agent_id: DEFAULT_AGENT_ID,
            turn_id: "t1",
            call_id: "c1",
            tool: "fs_write",
            skill: "",
            handoff: "",
            routine: "",
            decision: crate::audit::AuditDecision::AllowOnce,
            policy_reason: "this creates a file in the workspace",
            args: &serde_json::json!({ "path": "a.txt", "content": "x" }),
            outcome: crate::audit::Outcome::Ok,
            duration_ms: 1,
            bytes_in: 1,
            bytes_out: 0,
            error_code: None,
            artifact: None,
        });

        let second = AppState::new(dir.path());

        assert_eq!(
            second.audit().tail(10, None).expect("tail").len(),
            1,
            "the record of what was done outlives the process"
        );
        assert!(
            !second.grants().holds("s1", &crate::policy::Grant::FsWrite),
            "an allow-session grant must never survive a restart"
        );
    }

    /// A second row as a CLI login: keyless, so no test reaches the
    /// credential store for it.
    fn cli_row(state: &AppState, model: &str) -> String {
        state
            .settings()
            .add(&RowDraft {
                label: Some("Claude"),
                base_url: "",
                model,
                auth_kind: AuthKind::ClaudeCli,
                max_output_tokens: None,
            })
            .expect("added")
            .id
    }

    fn reviewer_on(state: &AppState, provider_id: &str, model: &str) -> Agent {
        state
            .create_agent(&AgentDraft {
                name: "Reviewer".to_owned(),
                role: "reads and reports".to_owned(),
                instructions: String::new(),
                provider_id: provider_id.to_owned(),
                model: model.to_owned(),
                tools: Vec::new(),
                skills: Vec::new(),
                runs_per_day: 0,
            })
            .expect("created")
    }

    /// PLAN 7.19's exit, end to end below the commands: an identity bound to a
    /// second row answers from it, a session overrides that without touching
    /// the identity, and clearing the override goes back.
    #[test]
    fn a_turn_answers_from_the_resolved_binding() {
        let dir = TempDir::new().expect("temp dir");
        let state = AppState::new(dir.path());
        let second = cli_row(&state, "claude-sonnet-4-6");
        let reviewer = reviewer_on(&state, &second, "claude-opus-4-1");
        let session = state
            .create_session("p1", None, Some(&reviewer.id), None, None)
            .expect("session");

        assert_eq!(
            state.provider_for(&reviewer, &session.id).model(),
            "claude-opus-4-1",
            "the identity's model"
        );

        let bound = state
            .set_session_binding(&session.id, None, Some("claude-haiku-4-5"))
            .expect("bound");
        assert_eq!(bound.model.as_deref(), Some("claude-haiku-4-5"));
        assert_eq!(bound.agent_id, reviewer.id, "the identity stays");
        assert_eq!(
            state.provider_for(&reviewer, &session.id).model(),
            "claude-haiku-4-5"
        );

        state
            .set_session_binding(&session.id, Some(DEFAULT_PROVIDER_ID), None)
            .expect("bound");
        assert_eq!(
            state.provider_for(&reviewer, &session.id).model(),
            crate::agent::provider::fake::FAKE_MODEL,
            "the unconfigured default row is the scripted provider"
        );

        let cleared = state
            .set_session_binding(&session.id, Some("  "), Some(""))
            .expect("cleared");
        assert_eq!((cleared.provider_id, cleared.model), (None, None));
        assert_eq!(
            state.provider_for(&reviewer, &session.id).model(),
            "claude-opus-4-1"
        );
        assert_eq!(
            state.agents().get(&reviewer.id).expect("kept"),
            reviewer,
            "no override rebinds the identity"
        );
    }

    #[test]
    fn a_session_can_be_created_with_an_override() {
        let dir = TempDir::new().expect("temp dir");
        let state = AppState::new(dir.path());
        let second = cli_row(&state, "claude-sonnet-4-6");

        let session = state
            .create_session("p1", None, None, Some(&second), None)
            .expect("session");
        assert_eq!(session.provider_id.as_deref(), Some(second.as_str()));
        assert_eq!(session.model, None);
        assert_eq!(
            state.provider_for(&Agent::builtin(), &session.id).model(),
            "claude-sonnet-4-6",
            "a row with no model override sends the row's"
        );

        let err = state
            .create_session("p1", None, None, Some("gone"), None)
            .expect_err("refused");
        assert_eq!(
            serde_json::to_value(&err).expect("serializes")["field"],
            "provider"
        );
    }

    #[test]
    fn a_binding_cannot_change_while_a_turn_runs() {
        let dir = TempDir::new().expect("temp dir");
        let state = AppState::new(dir.path());
        let session = state
            .create_session("p1", None, None, None, None)
            .expect("session");
        let _cancel = state.turns().begin(&session.id, "t1").expect("begun");

        let err = state
            .set_session_binding(&session.id, None, Some("m"))
            .expect_err("refused");
        assert_eq!(
            serde_json::to_value(&err).expect("serializes")["code"],
            "E_TURN_BUSY"
        );
        assert_eq!(
            state.sessions().binding_of(&session.id).expect("read"),
            (None, None)
        );
    }

    #[test]
    fn an_identity_cannot_name_a_row_that_is_not_on_file() {
        let dir = TempDir::new().expect("temp dir");
        let state = AppState::new(dir.path());
        let second = cli_row(&state, "m");
        let reviewer = reviewer_on(&state, &second, "");

        let err = state
            .update_agent(
                &reviewer.id,
                &AgentDraft {
                    name: reviewer.name.clone(),
                    role: reviewer.role.clone(),
                    instructions: String::new(),
                    provider_id: "gone".to_owned(),
                    model: String::new(),
                    tools: Vec::new(),
                    skills: Vec::new(),
                    runs_per_day: 0,
                },
            )
            .expect_err("refused");
        assert_eq!(
            serde_json::to_value(&err).expect("serializes")["field"],
            "provider"
        );
    }

    /// A row something answers from is not deleted, and nothing is moved to
    /// another row to make room.
    #[test]
    fn a_row_in_use_is_not_deleted() {
        let dir = TempDir::new().expect("temp dir");
        let state = AppState::new(dir.path());

        assert!(state.delete_provider(DEFAULT_PROVIDER_ID).is_err());

        let bound = cli_row(&state, "m");
        let reviewer = reviewer_on(&state, &bound, "");
        let overridden = cli_row(&state, "m");
        let session = state
            .create_session("p1", None, None, Some(&overridden), None)
            .expect("session");

        let err = state.delete_provider(&bound).expect_err("refused");
        assert!(
            matches!(
                err,
                AppError::ProviderInUse {
                    identities: 1,
                    sessions: 0
                }
            ),
            "{err}"
        );
        let err = state.delete_provider(&overridden).expect_err("refused");
        assert!(
            matches!(
                err,
                AppError::ProviderInUse {
                    identities: 0,
                    sessions: 1
                }
            ),
            "{err}"
        );
        assert_eq!(
            state.agents().get(&reviewer.id).expect("kept").provider_id,
            bound
        );

        state
            .set_session_binding(&session.id, None, None)
            .expect("cleared");
        state.delete_provider(&overridden).expect("deleted");
        assert!(!state.settings().contains(&overridden));
    }

    /// Clearing a key a CLI owns is refused: Aegis never stored it.
    #[test]
    fn a_cli_row_has_no_key_to_clear() {
        let dir = TempDir::new().expect("temp dir");
        let state = AppState::new(dir.path());
        let second = cli_row(&state, "m");

        let err = state.clear_provider_key(&second).expect_err("refused");
        assert_eq!(
            serde_json::to_value(&err).expect("serializes")["code"],
            "E_INVALID_SETTING"
        );
        assert!(matches!(
            state.clear_provider_key("gone"),
            Err(AppError::ProviderNotFound { .. })
        ));
    }
}
