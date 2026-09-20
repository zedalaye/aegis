//! OS notifications (PLAN 7.22).
//!
//! Nothing told a person anything unless the window was open. This is the one
//! way out of the process that does not need a window, and it is deliberately
//! thin:
//!
//! * **Called from Rust**, like the folder picker: `capabilities/main.json`
//!   gains no permission, and the WebView cannot raise a notification itself.
//! * **Not a dialog**: a click brings the window forward, and nothing is
//!   approved from a notification (PLAN 7.22, *Refuses*).
//! * **Never the arguments**: the content is the routine's name and one
//!   sentence. A path, a command line or an amount belongs in the card the
//!   window draws, not on a lock screen.
//! * **One line per routine per hour** ([`COALESCE`]): a routine that parks
//!   three calls is one notification, and what it held is counted into the
//!   next one.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Mutex, MutexGuard};

use tokio::time::{Duration, Instant};

/// How long one key stays quiet after a notification.
pub const COALESCE: Duration = Duration::from_secs(60 * 60);

/// Longest body a notification carries. A lock screen truncates anyway; this
/// decides where.
const BODY_MAX_CHARS: usize = 160;

/// One thing worth telling somebody who is not at the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// What is coalesced on: the routine's id, or the session's when no
    /// routine is behind it.
    pub key: String,
    /// The first line — the routine's name, or what this is about.
    pub title: String,
    /// One sentence. Never an argument, a path or an amount.
    pub body: String,
}

impl Note {
    /// A note about `key`, with its two lines.
    pub fn new(key: impl Into<String>, title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            title: title.into(),
            body: body.into(),
        }
    }
}

/// Where a [`Note`] goes. One implementation reaches the OS, one records (the
/// tests), one drops everything (a runtime with no window).
pub trait Notifier: Send + Sync + fmt::Debug {
    /// Posts a note, or holds it because its key is inside [`COALESCE`].
    fn post(&self, note: Note);
}

/// A notifier that says nothing. What a runtime with no window uses.
#[derive(Debug, Default)]
pub struct Quiet;

impl Notifier for Quiet {
    fn post(&self, note: Note) {
        tracing::debug!(key = %note.key, "a notification was dropped: nothing to post it with");
    }
}

/// What a key is allowed, and what it is holding.
#[derive(Debug)]
struct Window {
    /// When the last note for this key went out.
    sent: Instant,
    /// How many were held since.
    held: u32,
}

/// One note per key per [`COALESCE`], counting what it suppressed.
///
/// Separate from the posting itself so the rule is testable without an OS.
#[derive(Debug, Default)]
pub struct Coalescer {
    keys: Mutex<HashMap<String, Window>>,
}

impl Coalescer {
    /// An empty coalescer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Locks the map, recovering from a poisoned mutex.
    fn keys(&self) -> MutexGuard<'_, HashMap<String, Window>> {
        self.keys
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Whether this note goes out now, and how many were held before it.
    ///
    /// `None` holds it: the key spoke inside the last hour.
    pub fn admit(&self, key: &str) -> Option<u32> {
        let now = Instant::now();
        let mut keys = self.keys();

        match keys.get_mut(key) {
            Some(window) if now.duration_since(window.sent) < COALESCE => {
                window.held += 1;
                None
            }
            Some(window) => {
                window.sent = now;
                Some(std::mem::take(&mut window.held))
            }
            None => {
                keys.insert(key.to_owned(), Window { sent: now, held: 0 });
                Some(0)
            }
        }
    }
}

/// The notifier that reaches the OS.
///
/// Built per call like [`WindowSink`](crate::commands::session::WindowSink),
/// borrowing the process-wide [`Coalescer`] from
/// [`AppState`](crate::AppState): the hour is a property of the installation,
/// not of whichever task happened to raise the note.
pub struct Desktop<'a, R: tauri::Runtime> {
    app: tauri::AppHandle<R>,
    coalescer: &'a Coalescer,
}

impl<'a, R: tauri::Runtime> Desktop<'a, R> {
    /// Wraps a handle and the coalescer it shares with every other note.
    pub const fn new(app: tauri::AppHandle<R>, coalescer: &'a Coalescer) -> Self {
        Self { app, coalescer }
    }
}

impl<R: tauri::Runtime> fmt::Debug for Desktop<'_, R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Desktop").finish_non_exhaustive()
    }
}

impl<R: tauri::Runtime> Notifier for Desktop<'_, R> {
    fn post(&self, note: Note) {
        use tauri_plugin_notification::NotificationExt as _;

        let Some(held) = self.coalescer.admit(&note.key) else {
            tracing::debug!(key = %note.key, "a notification was held: this key spoke recently");
            return;
        };

        // A failure here is a machine that will not show notifications (no
        // permission, no toast registration): logged, never surfaced as an
        // error on whatever raised it.
        if let Err(err) = self
            .app
            .notification()
            .builder()
            .title(note.title)
            .body(body(&note.body, held))
            .show()
        {
            tracing::debug!(%err, "a notification could not be posted");
        }
    }
}

/// The body as it is posted: one sentence, cut to [`BODY_MAX_CHARS`], with
/// what was held while this key was quiet.
pub fn body(sentence: &str, held: u32) -> String {
    let sentence = sentence.trim();
    let mut out: String = sentence.chars().take(BODY_MAX_CHARS).collect();
    if out.chars().count() < sentence.chars().count() {
        out.push('…');
    }
    if held > 0 {
        out.push_str(&format!(" ({held} more since the last notification.)"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn one_key_speaks_once_an_hour_and_counts_what_it_held() {
        let coalescer = Coalescer::new();

        assert_eq!(coalescer.admit("r1"), Some(0), "the first one goes out");
        assert_eq!(coalescer.admit("r1"), None, "the second is held");
        assert_eq!(coalescer.admit("r1"), None);
        assert_eq!(
            coalescer.admit("r2"),
            Some(0),
            "another routine is not held by this one"
        );

        tokio::time::advance(COALESCE).await;
        assert_eq!(
            coalescer.admit("r1"),
            Some(2),
            "the hour is up, and what was held is counted"
        );
        assert_eq!(coalescer.admit("r1"), None, "the window starts again");
    }

    #[test]
    fn a_held_count_is_said_in_the_body() {
        assert_eq!(body(" it parked a write. ", 0), "it parked a write.");
        assert_eq!(
            body("it parked a write.", 2),
            "it parked a write. (2 more since the last notification.)"
        );
    }

    #[test]
    fn a_long_sentence_is_cut_rather_than_posted_whole() {
        let posted = body(&"a".repeat(BODY_MAX_CHARS + 40), 0);
        assert_eq!(posted.chars().count(), BODY_MAX_CHARS + 1);
        assert!(posted.ends_with('…'));
    }
}
