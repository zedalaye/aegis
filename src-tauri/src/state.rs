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

use crate::agent::provider::openai;
use crate::agent::{FakeProvider, OpenAiProvider, Provider, ProviderProbe, TurnRegistry};
use crate::approval::{ApprovalRegistry, ApprovalRequest, Decision, Resolution};
use crate::audit::AuditLog;
use crate::error::AppResult;
use crate::policy::GrantStore;
use crate::secrets::{key_hint, SecretStore};
use crate::store::{
    MaskedSettings, SessionDetail, SessionState, SessionStore, SessionSummary, SettingsStore, Store,
};

/// Shared state, registered with `Manager::manage` and read from commands via
/// `tauri::State<'_, AppState>`.
#[derive(Debug)]
pub struct AppState {
    started_at: Instant,
    quitting: AtomicBool,
    store: Store,
    sessions: SessionStore,
    settings: SettingsStore,
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
            store: Store::load(data_dir),
            sessions: SessionStore::load(data_dir),
            settings: SettingsStore::load(data_dir),
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

        Box::new(OpenAiProvider::new(
            self.http.clone(),
            &settings,
            self.secrets.inspect().key,
        ))
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

        MaskedSettings {
            base_url: provider.base_url,
            model: provider.model,
            key_source: held.source,
            key_hint: held.key.as_ref().map(|key| key_hint(key.expose())),
            keyring_available: held.keyring_available,
        }
    }

    /// Asks the configured server whether it is reachable and the key works.
    ///
    /// Lives here rather than in the command because it needs three things no
    /// one of them owns: the settings, the key, and the shared HTTP client.
    pub async fn probe_provider(&self) -> ProviderProbe {
        let settings = self.settings.get();
        let key = self.secrets.inspect().key;

        openai::probe(self.http.as_ref(), &settings, key.as_ref()).await
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
            turn_id: "t1",
            call_id: "c1",
            tool: "fs_list",
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
            .set("https://api.example.test/v1", "some-model")
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

        let rendered = serde_json::to_string(&masked).expect("serializes");
        assert!(!rendered.contains("api_key"), "{rendered}");
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
            turn_id: "t1",
            call_id: "c1",
            tool: "fs_write",
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
