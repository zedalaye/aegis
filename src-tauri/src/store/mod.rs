//! On-disk persistence: small, hand-editable JSON documents under the OS
//! application-data directory, one per store, each with its own schema version
//! (`docs/guide/data.md`).
//!
//! * **Writes are atomic**: temporary sibling, `sync_all`, rename.
//! * **A damaged document never blocks the app**: it is moved aside and the
//!   store starts empty.
//! * **Derived facts are never persisted**: workspace presence and running
//!   turns are measured on every read.

pub mod agents;
pub mod connectors;
pub mod memories;
pub mod parked;
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
pub use connectors::{Connector, ConnectorDraft, ConnectorStore};
pub use memories::{Memory, MemoryDraft, MemoryKind, MemoryStore};
pub use parked::{ParkCause, ParkDraft, ParkedAsk, ParkedStore};
pub use projects::{canonical_workspace, Project, ProjectDetail, Store};
pub use routines::{LastRun, Routine, RoutineDraft, RoutineStore, RunOutcome, Schedule};
pub use sessions::{
    Attachment, Compaction, Cost, Delegated, Message, Role, Scheduled, SessionDetail, SessionState,
    SessionStore, SessionSummary, ToolCallRecord, ToolCallStatus, TurnCost, TurnHandle,
};
pub use settings::{
    AuthKind, AuthPreset, Binding, BindingRequest, DecisionSettings, MaskedDecision,
    MaskedProvider, MaskedSettings, ProviderEntry, ProviderSettings, RowDraft, SettingsStore,
    PROVIDERS_MAX,
};

/// Rename attempts before a failed save gives up: on Windows an antivirus or
/// indexer can hold the old file for a few milliseconds
/// (`docs/troubleshooting.md`).
const RENAME_ATTEMPTS: u32 = 3;
const RENAME_BACKOFF: Duration = Duration::from_millis(20);

/// Now, as fixed-width UTC RFC3339 (`2026-08-28T09:41:07.412Z`), so timestamps
/// compare as strings.
fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Drops a leading UTF-8 BOM, which Windows editors add and `serde_json`
/// rejects. Aegis never writes one.
fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)
}

/// Moves a damaged document aside, keeping it for repair. Best effort.
fn quarantine(path: &Path) {
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let backup = path.with_extension(format!("corrupt-{stamp}.json"));

    match fs::rename(path, &backup) {
        Ok(()) => tracing::warn!(backup = %backup.display(), "a damaged document was moved aside"),
        Err(err) => tracing::error!(%err, "could not move a damaged document aside"),
    }
}

/// Writes `bytes` to `path` so readers see the old file or the whole new one:
/// a temporary file in the same directory, `sync_all` so the rename cannot
/// outrun the data, then a replacing rename. The temporary file is removed on
/// failure.
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
