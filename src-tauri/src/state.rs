//! Application-wide runtime state, managed by Tauri.
//!
//! Each concern owns its own lock, and no command holds two. This is also where
//! facts that span stores are composed — a [`SessionSummary`] needs both the
//! session document and the turn registry.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::agent::decision::{self, DecisionClient};
use crate::agent::provider::pricing::{self, PriceSuggestion};
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
use crate::notify::Coalescer;
use crate::oauth;
use crate::policy::GrantStore;
use crate::schedule::runner::Scheduler;
use crate::secrets::{self, key_hint, ApiKey, KeySource, SecretStore};
use crate::spend::Tariff;
use crate::store::{
    self, Agent, AgentDraft, AgentStore, AuthKind, Binding, BindingRequest, Connector,
    ConnectorStore, MaskedDecision, MaskedProvider, MaskedSettings, Memory, MemoryDraft,
    MemoryStore, ParkedAsk, ParkedStore, ProviderEntry, Routine, RoutineStore, RowDraft,
    SessionDetail, SessionState, SessionStore, SessionSummary, SettingsStore, SpendLedger, Store,
    DEFAULT_AGENT_ID, DEFAULT_PROVIDER_ID,
};

mod parked;
mod providers;
mod routines;

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
    /// The asks nobody could answer (PLAN 7.22), kept beside the other stores
    /// and never in a workspace.
    parked: ParkedStore,
    /// One notification per routine per hour, process-wide (PLAN 7.22).
    coalescer: Coalescer,
    /// Which routines are running now, so nothing fires twice (Phase 16). In
    /// memory only: nothing is running after a crash.
    scheduler: Scheduler,
    settings: SettingsStore,
    /// What every turn's model calls cost (PLAN 7.26). Written by the turn
    /// loop only.
    spend: SpendLedger,
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
            parked: ParkedStore::load(data_dir),
            coalescer: Coalescer::new(),
            scheduler: Scheduler::new(),
            settings: SettingsStore::load(data_dir),
            spend: SpendLedger::load(data_dir),
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

    /// The provider settings on disk.
    pub fn settings(&self) -> &SettingsStore {
        &self.settings
    }

    /// The spend ledger (PLAN 7.26).
    pub fn spend(&self) -> &SpendLedger {
        &self.spend
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
    /// cannot re-create what was cleared. Its parked asks go with it: an
    /// answer resumes a run, and there is nothing left to resume.
    pub fn close_session(&self, session_id: &str) {
        self.turns.forget(session_id);
        self.approvals.withdraw_session(session_id);
        self.grants.clear(session_id);
        self.parked.forget_session(session_id);
    }

    /// The asks waiting for a person (PLAN 7.22).
    pub fn parked(&self) -> &ParkedStore {
        &self.parked
    }

    /// What every notification is coalesced through.
    pub fn coalescer(&self) -> &Coalescer {
        &self.coalescer
    }

    /// One project's parked asks, oldest first.
    pub fn parked_list(&self, project_id: Option<&str>) -> Vec<ParkedAsk> {
        self.parked.list(project_id)
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
mod tests;
