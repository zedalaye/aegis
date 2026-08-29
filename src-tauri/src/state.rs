//! Application-wide runtime state, managed by Tauri.
//!
//! Anything a command needs and cannot derive from its arguments lives here,
//! behind `&self` so commands never take a lock they do not need. Each concern
//! owns its own synchronization rather than sharing one coarse mutex: Phase 2
//! adds the project store, and the policy engine, per-session grants and the
//! per-turn cancellation registry land the same way.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::audit::AuditLog;
use crate::policy::GrantStore;
use crate::store::Store;

/// Shared state, registered with `Manager::manage` and read from commands via
/// `tauri::State<'_, AppState>`.
#[derive(Debug)]
pub struct AppState {
    started_at: Instant,
    quitting: AtomicBool,
    store: Store,
    grants: GrantStore,
    audit: AuditLog,
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
            grants: GrantStore::new(),
            audit: AuditLog::new(data_dir),
        }
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
    pub fn begin_quit(&self) -> bool {
        !self.quitting.swap(true, Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

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
        });

        assert_eq!(log.tail(10, None).expect("tail").len(), 1);
        assert!(log.path().is_file(), "the line reached the data directory");
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
