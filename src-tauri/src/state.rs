//! Application-wide runtime state, managed by Tauri.
//!
//! Anything a command needs and cannot derive from its arguments lives here,
//! behind `&self` so commands never take a lock they do not need. Each concern
//! owns its own synchronization rather than sharing one coarse mutex: the
//! project store, the session store, per-session grants, the pending
//! approvals, the audit log and the turn registry are six independent locks,
//! and no command holds two.
//!
//! This is also the composition point for the one fact no single store can
//! answer on its own. A [`SessionSummary`] needs both the transcript (from the
//! session document) and whether a turn is running (from the registry), so the
//! methods that produce one live here rather than in either.

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

/// How many audit lines a board is folded from (PLAN 7.3, Phase 17).
///
/// The ceiling [`AuditLog::tail`](crate::audit::AuditLog::tail) will honour, and
/// the board asks for all of it: unlike the drawer, which is a tail somebody
/// scrolls, this is an aggregate, and a run whose first half fell outside the
/// window would be reported as a smaller run rather than as a partial one.
/// Bounded all the same — the log outlives any window over it, which is why
/// the panel says how far back it reaches.
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
    /// Which routines are running right now, so nothing fires twice
    /// (PLAN 7.3, Phase 16). In memory only: a routine is not running after a
    /// crash, and a persisted flag saying it was is what would stop a clock
    /// forever.
    scheduler: Scheduler,
    settings: SettingsStore,
    /// The connectors somebody configured (PLAN 7.3, Phase 18).
    connector_store: ConnectorStore,
    /// The connectors that are actually running.
    ///
    /// Two fields rather than one because they answer different questions and
    /// change at different rates. The document is what a person typed and it
    /// survives a restart; the roster is processes, and a process that was up
    /// when this one died is not up now. The Settings panel joins them
    /// ([`AppState::connector_views`]), which is the composition this module
    /// exists for.
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
}

impl AppState {
    /// Builds the state for a fresh process, loading persisted data from
    /// `data_dir` (the OS application-data directory).
    ///
    /// Infallible on purpose. A store that cannot be read yields an empty one
    /// and a log line; the app still boots, and the failure is reported to the
    /// user on the first save rather than as a window that never appears.
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
            // Empty at construction, on purpose: starting a program is async
            // and `new` is not, and a window that waited on four `npx` runs
            // before it appeared would be a worse first minute than four rows
            // that fill in. `commands::connector::spawn` starts them.
            connectors: Connectors::new(),
            secrets: SecretStore::new(),
            turns: TurnRegistry::new(),
            grants: GrantStore::new(),
            approvals: ApprovalRegistry::new(),
            audit: AuditLog::new(data_dir),
            // Built once and shared. Nothing here reads the credential store:
            // startup must not prompt for a keychain the user may never use in
            // this session.
            http: openai::client(),
            // Only used to refuse `shell_exec` on Aegis itself. A platform
            // that will not name its own executable loses that one check and
            // nothing else, so the failure is logged rather than propagated.
            self_exe: std::env::current_exe()
                .inspect_err(|err| {
                    tracing::warn!(%err, "could not resolve this executable's path");
                })
                .ok(),
            // Beside the stores, not inside the workspace (PLAN 5.4). Created
            // here rather than on first capture so that `lib.rs` has an
            // existing directory to scope the asset protocol to, and so a
            // user can find the folder before there is anything in it.
            captures: prepare_captures(data_dir),
            // Seeded on a first run only, and never argued about afterwards:
            // see `skills::seed`. Ordinary files in an ordinary directory,
            // which is what lets a user write, edit and delete a runbook with
            // their own editor.
            skills: prepare_skills(data_dir),
        }
    }

    /// The session store: transcripts and titles.
    pub fn sessions(&self) -> &SessionStore {
        &self.sessions
    }

    /// Which sessions are running, and how to cancel them.
    pub fn turns(&self) -> &TurnRegistry {
        &self.turns
    }

    /// Who answers a turn, decided fresh for each one.
    ///
    /// Per turn rather than once per process, and that is the point: settings
    /// can change between two messages in the same session, a key can be added
    /// or cleared, and a provider captured at startup would keep answering
    /// from a configuration the user has already moved on from.
    ///
    /// Unconfigured settings mean the scripted provider — a fresh install
    /// still streams a reply and still walks the approval gate, which is the
    /// documented Phase 5 behaviour rather than a fault. Configured settings
    /// mean the real one *even with no key*: the user asked for a model, and
    /// answering them with the fake instead of `E_NO_API_KEY` would be a lie
    /// they could not see through (see
    /// [`ProviderSettings::is_configured`](crate::store::ProviderSettings::is_configured)).
    ///
    /// A `Box` because the choice is made here and the value has to outlive
    /// the call; the turn loop still takes `&dyn Provider` and still cannot
    /// tell which one it was handed. A roster of providers later is a
    /// different decision inside this one function (PLAN 7.1).
    pub fn provider(&self) -> Box<dyn Provider> {
        let settings = self.settings.get();

        if !settings.is_configured() {
            return Box::new(FakeProvider::new());
        }

        // Everything motosan speaks goes to motosan: the three CLI logins,
        // Gemini, and an API key aimed at Anthropic's own host. That last one
        // used to fall through to the OpenAI-compatible path, which reaches
        // the vendor's compatibility shim — a shim that drops prompt caching,
        // so every round of every turn re-sent the whole transcript at full
        // price.
        if catalog::uses_motosan(settings.auth_kind, &settings.base_url) {
            // A CLI login's credential is on disk; only the key path needs the
            // secret store, and reading it for the others would be a keyring
            // prompt bought for nothing. Gemini is a key, not a login.
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

    /// Who answers for one identity (PLAN 7.3, Phase 12).
    ///
    /// This build resolves exactly one binding — [`DEFAULT_PROVIDER_ID`], the
    /// provider named in Settings — and the store refuses to save an identity
    /// bound to anything else, so the fallback below is reachable only by hand
    /// editing `agents.json`. It falls back rather than failing because a
    /// session that cannot be talked to at all is a worse answer than one
    /// answered by the provider the user configured, and the warning says which
    /// happened.
    ///
    /// This function is the roster's seam. A second provider later is a second
    /// arm here plus a settings row — not a change to the turn loop, which
    /// still takes a `&dyn Provider` and still cannot tell which it was handed.
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

    /// The provider settings, and everything that may be said about the key.
    ///
    /// The second composition this module exists for on the settings side: the
    /// base URL and the model come off disk, the key facts come from the
    /// platform, and neither store can answer for the other. One credential
    /// read serves all three key fields (see
    /// [`SecretStore::inspect`](crate::secrets::SecretStore::inspect)).
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
    ///
    /// Lives here rather than in the command because it needs three things no
    /// one of them owns: the settings, the key, and the shared HTTP client.
    pub async fn probe_provider(&self) -> ProviderProbe {
        let settings = self.settings.get();

        // The same fork as `provider`, and for the reason the probe exists: a
        // test that reaches a different endpoint than a turn does is a test
        // that can pass on a configuration that cannot answer.
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

    /// The largest reply the named model will produce, from the provider's own
    /// catalog.
    ///
    /// `pending_key` is the key from the form, which may not be the one in the
    /// credential store yet. It wins when it is there, so the save that first
    /// configures a provider can still read its catalog.
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

    /// Asks the current authentication kind which models it will accept.
    ///
    /// `base_url` is the one in the form, which may not have been saved yet —
    /// changing the URL and refreshing the list should describe that URL.
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

    /// The API key, wherever this machine keeps it.
    ///
    /// Deliberately not part of [`Store`]: the key is the one piece of
    /// configuration Aegis does not persist itself, and routing it through the
    /// same type as the project list is how it would end up in a JSON file
    /// next to them.
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

    /// The identity a session runs as.
    ///
    /// The third composition this module exists for: which identity a session
    /// named is in the session document, and what that identity *is* is in the
    /// agent document. Infallible for the reason
    /// [`AgentStore::resolve`](crate::store::AgentStore::resolve) is — this is
    /// called on the way into a turn, and a turn that would not start because
    /// a lookup failed is a session that can no longer be talked to. A session
    /// that has itself gone resolves to the built-in identity, and the turn
    /// fails a moment later on the transcript it cannot read, which is the
    /// failure worth reporting.
    pub fn agent_of(&self, session_id: &str) -> Agent {
        let named = self.sessions.agent_of(session_id).unwrap_or_else(|err| {
            tracing::warn!(%err, session_id, "no session to resolve an identity for");
            None
        });

        self.agents.resolve(named.as_deref())
    }

    /// Creates a session bound to an identity.
    ///
    /// The identity is checked here rather than in the session store, which
    /// cannot see the agent document. Checked at all because a session bound to
    /// an identity that does not exist is one that can talk and never act, and
    /// finding that out on the first tool call is finding it out too late.
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

    /// Deletes an identity, unless sessions still run as it.
    ///
    /// The check is here for the same reason the one above is: the agent store
    /// cannot see the session document. Refused rather than cascaded — see
    /// [`AppError::AgentInUse`](crate::AppError::AgentInUse).
    ///
    /// Its memories go with it, and that direction is deliberate: a memory is
    /// only ever reachable through the identity that holds it, so memories of a
    /// deleted identity are unreachable records that would grow the document
    /// forever. Cascading here is not the same decision as refusing above —
    /// what is refused there is orphaning a *transcript*, which is a record of
    /// what happened and belongs to the user.
    ///
    /// The identity goes first. If forgetting then failed, the result would be
    /// records nobody can read; the other order would leave an identity that
    /// has already lost what it knew.
    pub fn delete_agent(&self, agent_id: &str) -> AppResult<()> {
        let bound = self.sessions.count_for_agent(agent_id);
        if bound > 0 {
            return Err(AppError::AgentInUse { count: bound });
        }
        // A clock pointing at nobody is worse than a refusal: it would keep a
        // routine on the panel that can never run again, and the person who
        // deleted the identity is the one who knows whether the routine should
        // move or go (PLAN 7.3, Phase 16).
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

    /// Every connector's row: the record, joined to what is measured about it.
    ///
    /// The composition this module exists for, on the connector side. The store
    /// cannot say whether a process is up and the roster cannot say what
    /// somebody typed, and a row that showed only one of the two would be
    /// either a list of settings nobody can act on or a list of processes
    /// nobody configured.
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

    /// Every routine, each carrying whatever is wrong with it right now.
    ///
    /// The measuring is here because it is the composition no single store can
    /// do: the identity is in one document, the folder in another, the runbook
    /// on disk, and the answer changes without the routine being touched.
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

    /// Records where a watching routine's folder stands right now.
    ///
    /// Called when a routine is saved, so that "save it, then drop a file in"
    /// fires on the next tick rather than the one after — the first look is
    /// what a routine compares against, and doing it here means the routine
    /// starts from the moment somebody set it up rather than from whenever the
    /// clock next came round.
    ///
    /// Best effort, and silent: a folder that is not there yet is a routine
    /// with nothing to compare against, which the tick handles by learning
    /// ([`schedule::learning`](crate::schedule::learning)). Nothing about a
    /// clock schedule is touched.
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

    /// Why this routine cannot fire as it stands, or `None` when it can.
    ///
    /// Read by the panel on every list and by the scheduler on every tick, so
    /// a routine that is drawn as runnable is one that would actually run.
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

    /// The project's status board (PLAN 7.3, Phase 17).
    ///
    /// The largest composition in this module, and it is here for the reason
    /// the others are: no store can answer it. The file half is in the user's
    /// folder, the live half is spread over the session document, the turn
    /// registry, the approval registry and the routine document, and the runs
    /// are folded out of a log none of them can see.
    ///
    /// Everything is measured on the way through — a routine's problem, a
    /// session's state, a workspace that may have been unplugged since the last
    /// read — so a board is what is true when it is asked for, not what was
    /// true when something last changed.
    pub fn board(&self, project_id: &str) -> board::Board {
        let sessions = self.session_list(project_id);
        let routines: Vec<Routine> = self
            .routine_list()
            .into_iter()
            .filter(|routine| routine.project_id == project_id)
            .collect();

        // Only this project's questions. The registry is global — a dialog is
        // raised by a turn, and a turn belongs to a session — so the filter is
        // the session list that was just measured.
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

    /// One run, and the lines it is replayed from (PLAN 7.2, row 10).
    ///
    /// The same fold as the board, narrowed to one reference, plus the entries
    /// themselves in the order they happened. Re-folded rather than remembered:
    /// a board this window read a minute ago is not a thing to serve a detail
    /// out of, and the log is cheap to read again.
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

    /// The window of the audit log a board is folded from.
    ///
    /// A failure to read is an empty window rather than an error: a board whose
    /// file half is fine should still draw, and the drawer is where a log that
    /// will not read is reported.
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

    /// A project's workspace folder, when it is there right now.
    ///
    /// The project-shaped half of [`AppState::workspace_of`], which answers the
    /// same question for a session. A routine names a project rather than a
    /// session — its runs each open one — so it needs this one.
    pub fn workspace_for_project(&self, project_id: &str) -> Option<PathBuf> {
        self.store
            .list()
            .into_iter()
            .find(|project| project.id == project_id)
            .filter(|project| project.workspace_exists)
            .map(|project| PathBuf::from(project.workspace_path))
    }

    /// One identity's memories, most recently touched first.
    ///
    /// Takes an identity rather than defaulting to one: "whose memory" is the
    /// whole question, and a panel that guessed would be showing somebody
    /// else's.
    pub fn memory_list(&self, agent_id: &str) -> AppResult<Vec<Memory>> {
        // Checked so that a stale picker says "no such identity" rather than
        // drawing an empty list that looks like an identity which has learned
        // nothing.
        let agent = self.agents.get(agent_id)?;
        Ok(self.memories.list_for(&agent.id))
    }

    /// Records or corrects a memory, on the user's own account.
    ///
    /// The other half of `COS.md`'s *forget*: what the model may do is write
    /// and read, under the approval gate; correcting what it got wrong is the
    /// human's, and this is where that lands.
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

    /// Folds a session's older turns into state, and hands back the session as
    /// it now reads (PLAN 7.3, Phase 14).
    ///
    /// Refused while a turn is running, with the same `E_TURN_BUSY` a second
    /// send gets. The turn loop folds once, before its first request, precisely
    /// so that a turn's rounds all reason against the same context; a fold
    /// landing between two of them would show the model one history and then
    /// judge its next move against another. The window hides the button while a
    /// reply streams, but that is how the interface tells the truth, not how
    /// the rule holds — this is where it holds.
    ///
    /// Otherwise it always returns the detail, whether or not anything moved. A
    /// session with too few turns to fold is not a failure; it is an answer,
    /// and one the panel draws by finding the fold still absent.
    pub fn compact_session(&self, session_id: &str) -> AppResult<SessionDetail> {
        if let Some(turn_id) = self.turns.active_turn(session_id) {
            return Err(AppError::TurnBusy { turn_id });
        }

        self.sessions.compact(session_id, true)?;
        self.session_detail(session_id)
    }

    /// The user's skill library (PLAN 7.3, Phase 13).
    ///
    /// Beside the stores rather than inside a workspace, because a runbook
    /// like "never send without review" is a fact about how this person works,
    /// not about one project. The other scope — runbooks that *are* about one
    /// project — lives in that project's folder and travels with it.
    pub fn skills(&self) -> &Path {
        &self.skills
    }

    /// The skills one identity may run, in one workspace, right now.
    ///
    /// The composition this module exists for, on the skill side: the library
    /// is Aegis', the `skills/` directory is the project's, and the allow-list
    /// is the identity's. No single one of the three can answer on its own.
    pub fn skill_catalog(&self, workspace: Option<&Path>) -> Vec<crate::skills::Skill> {
        crate::skills::catalog(&self.skills, workspace)
    }

    /// This application's own binary, when the platform would name it.
    pub fn self_exe(&self) -> Option<&Path> {
        self.self_exe.as_deref()
    }

    /// Where `screen_capture` writes its PNGs.
    ///
    /// The one directory the WebView is allowed to read files from, and only
    /// through the asset protocol (see `lib.rs`). Never inside a workspace: a
    /// capture is an artefact of the harness, and one written into a project
    /// folder would end up in someone's next commit.
    pub fn captures(&self) -> &Path {
        &self.captures
    }

    /// A project's sessions, most recently active first, at their live states.
    ///
    /// The composition this module exists for: the rows come from the session
    /// document, the `state` on each comes from the turn registry.
    pub fn session_list(&self, project_id: &str) -> Vec<SessionSummary> {
        self.sessions.list(project_id, &self.turns.lookup())
    }

    /// One session's row, at its live state.
    pub fn session_summary(&self, session_id: &str) -> AppResult<SessionSummary> {
        self.sessions
            .summary(session_id, self.turns.state_of(session_id))
    }

    /// One session with its transcript, at its live state, plus whatever it
    /// is blocked on.
    ///
    /// The second composition this module exists for. The transcript comes off
    /// disk, the state comes from the turn registry, and the pending approvals
    /// come from the approval registry — which is what lets a window reopened
    /// mid-turn redraw a dialog it never saw raised, instead of leaving a turn
    /// waiting on a prompt nobody can answer.
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

    /// Forgets a session entirely: its turn, its grants and its approvals.
    ///
    /// Ordered deliberately. The turn is cancelled first so it stops making
    /// new calls; then its approvals go, which releases it if it was parked on
    /// one; then its grants, which nothing can consult once there is no turn.
    /// Doing it the other way round leaves a window in which a running turn
    /// re-creates what was just cleared.
    pub fn close_session(&self, session_id: &str) {
        self.turns.forget(session_id);
        self.approvals.withdraw_session(session_id);
        self.grants.clear(session_id);
    }

    /// The workspace a session's tools may touch.
    ///
    /// `Ok(None)` is a real answer, not a failure: the project exists but its
    /// folder is gone — unmounted, moved, renamed. Policy turns that into a
    /// hard `E_NO_WORKSPACE` denial for every call (PLAN 3.2), and the system
    /// message tells the model plainly rather than letting it discover the
    /// state one refusal at a time.
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

    /// The live `allow_session` grants, keyed by session.
    ///
    /// Deliberately not part of [`Store`]: a grant is a decision about the
    /// session a user is currently looking at, and it expires with the
    /// process. Persisting one would quietly turn "allow for this session"
    /// into "allow forever", which the MVP does not offer (PLAN 3.1).
    pub fn grants(&self) -> &GrantStore {
        &self.grants
    }

    /// The approvals open right now, keyed by request id.
    ///
    /// Not persisted, for the same reason grants are not, and one step
    /// stronger: a pending approval is a turn parked on a channel. Nothing
    /// survives the process that could be released by answering it after a
    /// restart, so offering the answer would be offering to approve a call
    /// that will never run.
    pub fn approvals(&self) -> &ApprovalRegistry {
        &self.approvals
    }

    /// The audit log every tool call writes to.
    ///
    /// One log for the whole process rather than one per session: the file is
    /// append-only and every line carries its `session_id`, so filtering is a
    /// read-time concern, and a single file is what a user can open, tail or
    /// ship to someone without first working out which of twenty files holds
    /// the call they are looking for.
    pub fn audit(&self) -> &AuditLog {
        &self.audit
    }

    /// How long this process has been up. Used by logging and, later, by the
    /// diagnostics surface.
    pub fn uptime(&self) -> Duration {
        self.started_at.elapsed()
    }

    /// Whether a real shutdown is in progress.
    ///
    /// This is the difference between the two ways the main window can be
    /// asked to close. A window-manager close is a *hide* — Aegis is a tray
    /// app and stays resident. An explicit quit has to be allowed through, so
    /// the close handler consults this flag instead of trapping every close
    /// and stranding the process alive.
    pub fn is_quitting(&self) -> bool {
        self.quitting.load(Ordering::SeqCst)
    }

    /// Whether the tray icon is actually up.
    ///
    /// False until setup installs it, and stays false when the platform has
    /// no AppIndicator library — in which case the window is the only
    /// surface and closing it must end the process.
    pub fn has_tray(&self) -> bool {
        self.tray.load(Ordering::SeqCst)
    }

    /// Records that the tray icon was installed. Called once from setup.
    pub fn mark_tray(&self) {
        self.tray.store(true, Ordering::SeqCst);
    }

    /// Marks shutdown as started; returns `true` if this call is the one that
    /// started it, so a double-quit does not run teardown twice.
    ///
    /// Running turns are cancelled on the way out. A task killed mid-write
    /// would leave a transcript with an assistant message whose tool calls are
    /// never answered; cancelling gives the loop the chance to answer them
    /// itself (see [`agent::turn`](crate::agent::turn)).
    pub fn begin_quit(&self) -> bool {
        let first = !self.quitting.swap(true, Ordering::SeqCst);
        if first {
            self.turns.cancel_all();
        }
        first
    }
}

/// Creates the skill library and puts the one example runbook in it.
///
/// Not canonicalized, unlike the capture directory: nothing matches this path
/// against a scope, it is only read from, and a canonical form would only
/// change what a tracing line prints. A directory that cannot be created
/// leaves a library with nothing in it, which reads as a catalog with nothing
/// in it — the honest answer, and one the panel can render.
fn prepare_skills(data_dir: &Path) -> PathBuf {
    let library = data_dir.join(crate::skills::LIBRARY_DIR);
    crate::skills::seed(&library);
    library
}

/// Creates the capture directory and resolves it to its canonical form.
///
/// Canonical because Tauri's asset-protocol scope canonicalizes the path the
/// WebView asks for before matching it against what was allowed; a scope
/// registered under a path with a symlink or a Windows short name in it would
/// match nothing, and every thumbnail would silently 403.
///
/// A directory that cannot be created is not a reason to refuse to start: the
/// failure surfaces on the first capture, which is where a user can do
/// something about it, rather than as a window that never appears.
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
                crate::skills::COS_SKILL,
                crate::skills::DEPLOY_SKILL,
                crate::skills::MAIL_SKILL,
                crate::skills::REVIEW_SKILL,
                crate::skills::REPLY_SKILL,
                crate::skills::REVIEW_DIFF_SKILL,
                crate::skills::THREAD_SKILL,
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

    /// The audit log is only useful if the one the runtime hands out is the
    /// one on disk. Everything else about auditing is tested against a log
    /// built directly; this is the seam where a wrong directory would send
    /// every line somewhere nobody looks.
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

    /// Which provider answers is decided from settings, per turn. A fresh
    /// install streams from the scripted provider — the documented Phase 5
    /// behaviour — and naming a base URL and a model is what switches it.
    ///
    /// Both halves are asserted through `model()`, which is the one thing the
    /// two providers cannot agree on. The configured half reads the
    /// credential store once; that is a lookup of an entry these tests never
    /// write, and nothing here can leave a key behind on the machine.
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
