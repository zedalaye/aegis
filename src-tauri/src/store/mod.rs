//! On-disk persistence.
//!
//! Six JSON documents under the OS application-data directory:
//! `projects.json` ([`projects`]), `sessions.json` ([`sessions`]),
//! `settings.json` ([`settings`]), from Phase 12 `agents.json` ([`agents`]),
//! from Phase 14 `memories.json` ([`memories`]) and from Phase 16
//! `routines.json` ([`routines`]). All of them are small,
//! human-readable and hand-editable on purpose — a user who has to recover from a bad state should be able to open
//! the file and see why.
//!
//! Three properties matter more than the format, and this module is where they
//! are implemented once for both documents:
//!
//! * **Writes are atomic.** A document is written to a sibling temporary file,
//!   flushed, then renamed over the target. A crash or a power cut leaves
//!   either the old document or the new one, never a half-written one.
//! * **A damaged document never blocks the app.** Unparseable content is moved
//!   aside with a timestamped name and the app starts with an empty list,
//!   because a tray app that refuses to boot has no way to tell anyone why.
//! * **Derived facts are never persisted.** Whether a workspace folder is
//!   still there, and whether a session has a turn running, are facts about
//!   *right now*. They are measured on every read. A stored copy would be
//!   wrong the moment a drive is unplugged or the process is killed mid-turn.
//!
//! The documents are deliberately separate files with separate schema
//! versions. They change at very different rates — a project list is edited by
//! a human a few times a week, a transcript grows on every token, provider
//! settings change a few times a year, identities barely at all — and a
//! migration to one has no business quarantining the others.

pub mod agents;
pub mod memories;
pub mod projects;
pub mod routines;
pub mod sessions;
pub mod settings;

use std::fs;
use std::io::{self, Write as _};
use std::path::Path;
use std::time::Duration;

use chrono::{SecondsFormat, Utc};

pub use agents::{Agent, AgentDraft, AgentStore, DEFAULT_AGENT_ID, DEFAULT_PROVIDER_ID};
pub use memories::{Memory, MemoryDraft, MemoryKind, MemoryStore};
pub use projects::{canonical_workspace, Project, ProjectDetail, Store};
pub use routines::{LastRun, Routine, RoutineDraft, RoutineStore, RunOutcome, Schedule};
pub use sessions::{
    Compaction, Cost, Delegated, Message, Role, Scheduled, SessionDetail, SessionState,
    SessionStore, SessionSummary, ToolCallRecord, ToolCallStatus, TurnCost, TurnHandle,
};
pub use settings::{AuthKind, AuthPreset, MaskedSettings, ProviderSettings, SettingsStore};

/// Rename attempts before a failed save gives up.
///
/// The replace step is a single `MoveFileEx` on Windows, which an antivirus or
/// an indexer holding the old file open can make fail for a few milliseconds
/// (see the README's Windows notes). Retrying briefly turns a transient
/// scanner collision back into a successful save.
const RENAME_ATTEMPTS: u32 = 3;
const RENAME_BACKOFF: Duration = Duration::from_millis(20);

/// Now, as fixed-width UTC RFC3339 (`2026-08-28T09:41:07.412Z`).
///
/// Fixed width and a fixed offset are what let timestamps be compared as
/// strings, both here and in the UI.
fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Drops a leading UTF-8 byte-order mark.
///
/// JSON has no BOM, and `serde_json` rejects one outright. Windows editors —
/// Notepad, and PowerShell's `Set-Content -Encoding utf8` — write one anyway,
/// so a user who takes up the invitation to edit `projects.json` by hand would
/// otherwise watch their project list get quarantined for a change they cannot
/// see. Aegis never writes a BOM; it only tolerates one.
fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)
}

/// Moves a document aside so a fresh one can be written.
///
/// Best effort by design: the caller is already on the "the store is
/// unusable" path, and failing to rename it must not stop the app from
/// starting. The original is kept rather than deleted — it may be the only
/// copy of what it held, and a human may well be able to repair it.
fn quarantine(path: &Path) {
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let backup = path.with_extension(format!("corrupt-{stamp}.json"));

    match fs::rename(path, &backup) {
        Ok(()) => tracing::warn!(backup = %backup.display(), "a damaged document was moved aside"),
        Err(err) => tracing::error!(%err, "could not move a damaged document aside"),
    }
}

/// Writes `bytes` to `path` so that readers see either the old file or the
/// whole new one.
///
/// Temporary file in the same directory (a rename across filesystems is not
/// atomic), `sync_all` before the rename (a rename can otherwise outrun the
/// data and survive a crash pointing at empty content), then a replacing
/// rename. The temporary file is removed if the rename never succeeds, so a
/// failing store does not leave litter behind.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("the store path has no parent directory"))?;
    fs::create_dir_all(dir)?;

    let tmp = path.with_extension("json.tmp");
    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }

    let mut last = None;
    for attempt in 0..RENAME_ATTEMPTS {
        match fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(err) => {
                last = Some(err);
                if attempt + 1 < RENAME_ATTEMPTS {
                    std::thread::sleep(RENAME_BACKOFF);
                }
            }
        }
    }

    let _ = fs::remove_file(&tmp);
    Err(last.unwrap_or_else(|| io::Error::other("the store could not be replaced")))
}
