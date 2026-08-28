//! Application-wide runtime state, managed by Tauri.
//!
//! Anything a command needs and cannot derive from its arguments lives here,
//! behind `&self` so commands never take a lock they do not need. Phase 1
//! carries only what the window lifecycle requires; later phases add the
//! project/session stores, the policy engine, per-session grants and the
//! per-turn cancellation registry as separate fields with their own locks,
//! rather than one coarse mutex around everything.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Shared state, registered with `Builder::manage` and read from commands via
/// `tauri::State<'_, AppState>`.
#[derive(Debug)]
pub struct AppState {
    started_at: Instant,
    quitting: AtomicBool,
}

impl AppState {
    /// Builds the state for a fresh process.
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            quitting: AtomicBool::new(false),
        }
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

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quitting_latches_once() {
        let state = AppState::new();
        assert!(!state.is_quitting());
        assert!(state.begin_quit(), "first call wins");
        assert!(state.is_quitting());
        assert!(!state.begin_quit(), "second call is a no-op");
        assert!(state.is_quitting());
    }
}
