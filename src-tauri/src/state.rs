//! Application-wide runtime state, managed by Tauri.
//!
//! Each concern owns its own lock, and no command holds two. This is also where
//! facts that span stores are composed — a [`SessionSummary`] needs both the
//! session document and the turn registry.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

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
use crate::secrets::{key_hint, SecretStore};
use crate::store::{
    Agent, AgentStore, AuthKind, Connector, ConnectorStore, MaskedSettings, Memory, MemoryDraft,
    MemoryStore, Routine, RoutineStore, SessionDetail, SessionState, SessionStore, SessionSummary,
    SettingsStore, Store, DEFAULT_AGENT_ID, DEFAULT_PROVIDER_ID,
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

    /// Who answers a turn, decided per turn so a settings change applies to the
    /// next message.
    ///
    /// Unconfigured settings get the scripted provider. Configured settings get
    /// the real one even with no key, so the user sees `E_NO_API_KEY` rather than
    /// a silent fake
    /// ([`ProviderSettings::is_configured`](crate::store::ProviderSettings::is_configured)).
    /// A provider roster would be decided here (PLAN 7.1).
    pub fn provider(&self) -> Box<dyn Provider> {
        let settings = self.settings.get();

        if !settings.is_configured() {
            return Box::new(FakeProvider::new());
        }

        // motosan handles the CLI logins, Gemini, and an API key for
        // Anthropic's own host, whose compatibility layer drops prompt caching.
        if catalog::uses_motosan(settings.auth_kind, &settings.base_url) {
            // Only a key needs the secret store; a CLI login reads its own file.
            let key = if settings.auth_kind.is_cli() {
                None
            } else {
                self.secrets.inspect().key
            };

            return Box::new(SubscriptionProvider::new(settings, key, self.http.clone()));
        }

        Box::new(OpenAiProvider::new(
            self.http.clone(),
            &settings,
            self.secrets.inspect().key,
        ))
    }

    /// Who answers for one identity (Phase 12). Only [`DEFAULT_PROVIDER_ID`]
    /// exists; any other binding (a hand-edited `agents.json`) falls back with a
    /// warning. A provider roster is a second arm here, not a turn-loop change.
    pub fn provider_for(&self, agent: &Agent) -> Box<dyn Provider> {
        if agent.provider_id != DEFAULT_PROVIDER_ID {
            tracing::warn!(
                agent = %agent.name,
                provider_id = %agent.provider_id,
                "this build has one provider; answering from the configured one"
            );
        }

        self.provider()
    }

    /// The provider settings and what may be shown about the key: settings from
    /// disk, key facts from one read of the platform store
    /// ([`SecretStore::inspect`](crate::secrets::SecretStore::inspect)).
    pub fn masked_settings(&self) -> MaskedSettings {
        let provider = self.settings.get();
        let held = self.secrets.inspect();

        let (key_source, key_hint) = if provider.auth_kind.is_cli() {
            let source = match provider.auth_kind {
                AuthKind::ClaudeCli => crate::secrets::KeySource::ClaudeCli,
                AuthKind::CodexCli => crate::secrets::KeySource::CodexCli,
                AuthKind::GrokCli => crate::secrets::KeySource::GrokCli,
                AuthKind::ApiKey | AuthKind::Gemini => crate::secrets::KeySource::None,
            };
            let hint =
                oauth::peek(provider.auth_kind).map(|peek| key_hint(peek.access_token.expose()));
            (source, hint)
        } else {
            (
                held.source,
                held.key.as_ref().map(|key| key_hint(key.expose())),
            )
        };

        MaskedSettings {
            auth_kind: provider.auth_kind,
            base_url: provider.base_url,
            model: provider.model,
            max_output_tokens: provider.max_output_tokens,
            key_source,
            key_hint,
            keyring_available: held.keyring_available,
            presets: AuthKind::presets().to_vec(),
        }
    }

    /// Asks the configured server whether it is reachable and the key works.
    pub async fn probe_provider(&self) -> ProviderProbe {
        let settings = self.settings.get();

        // The same fork as `provider`: the probe must reach what a turn would.
        if catalog::uses_motosan(settings.auth_kind, &settings.base_url) {
            let key = if settings.auth_kind.is_cli() {
                None
            } else {
                self.secrets.inspect().key
            };

            return motosan::probe(
                settings.auth_kind,
                &settings.model,
                &settings.base_url,
                key,
                self.http.as_ref(),
            )
            .await;
        }

        let key = self.secrets.inspect().key;
        openai::probe(self.http.as_ref(), &settings, key.as_ref()).await
    }

    /// The named model's largest reply, from the provider's catalog.
    /// `pending_key`, from a form not saved yet, wins over the stored key.
    pub async fn model_output_cap(
        &self,
        kind: AuthKind,
        base_url: &str,
        model: &str,
        pending_key: Option<&str>,
    ) -> Option<u32> {
        let key = pending_key
            .and_then(crate::secrets::ApiKey::new)
            .or_else(|| self.secrets.inspect().key);

        catalog::output_cap(kind, base_url, model, self.http.as_ref(), key.as_ref()).await
    }

    /// The models an authentication kind accepts, for the base URL in the form.
    pub async fn list_models(&self, kind: AuthKind, base_url: &str) -> ModelCatalog {
        catalog::list(
            kind,
            base_url,
            self.http.as_ref(),
            self.secrets.inspect().key.as_ref(),
        )
        .await
    }

    /// The provider settings on disk.
    pub fn settings(&self) -> &SettingsStore {
        &self.settings
    }

    /// The API key, wherever this machine keeps it — never in a document Aegis
    /// writes.
    pub fn secrets(&self) -> &SecretStore {
        &self.secrets
    }

    /// The identities on disk, and the built-in one that is not.
    pub fn agents(&self) -> &AgentStore {
        &self.agents
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
    /// store cannot see the agent document.
    pub fn create_session(
        &self,
        project_id: &str,
        title: Option<&str>,
        agent_id: Option<&str>,
    ) -> AppResult<SessionSummary> {
        let agent_id = agent_id.unwrap_or(DEFAULT_AGENT_ID);
        let agent = self.agents.get(agent_id)?;

        self.sessions.create(project_id, title, &agent.id)
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
    let captures = data_dir.join("captures");

    if let Err(err) = std::fs::create_dir_all(&captures) {
        tracing::warn!(%err, dir = %captures.display(), "could not create the capture directory");
        return captures;
    }

    dunce::canonicalize(&captures).unwrap_or(captures)
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
        assert_eq!(masked.base_url, "");
        assert_eq!(masked.model, "");
        assert_eq!(masked.auth_kind, crate::store::AuthKind::ApiKey);

        let rendered = serde_json::to_string(&masked).expect("serializes");
        assert!(
            !rendered.contains("\"api_key\":"),
            "the payload must not have a key field: {rendered}"
        );
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
}
