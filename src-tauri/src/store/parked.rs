//! Parked asks: `parked.json` (PLAN 7.22).
//!
//! An ask nobody could answer used to be a refusal, and the run's work was
//! lost until the next fire. A park is the third answer: the call did not run,
//! the question survives, and a person answers it from the board.
//!
//! * **Never in the workspace**: this document sits beside the other stores in
//!   the application data directory. A park is a fact about this installation,
//!   not about the project's files (PLAN 7.22, *Refuses*).
//! * **The fingerprint is the key**: the tool plus the audit log's digest of
//!   the arguments. An answer matches that exact call and nothing else, so a
//!   model that regenerates different arguments asks again.
//! * **Bounded**: [`MAX_PARKS_PER_RUN`] per run, and a park nobody answers for
//!   [`PARK_TTL_DAYS`] closes as `blocked`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use super::{now, quarantine, strip_bom, write_atomic};
use crate::error::{AppError, AppResult};
use crate::policy::{ApprovalDetail, Grant, Risk};

/// Name of the document under the application-data directory.
const PARKED_FILE: &str = "parked.json";

/// Schema version of [`ParkedFile`].
const SCHEMA_VERSION: u32 = 1;

/// Most calls one run may park. Past it a run is not asking questions any
/// more, it is trying to get through the gate one call at a time.
pub const MAX_PARKS_PER_RUN: usize = 3;

/// How long a parked ask stays answerable. Past it the question is stale: the
/// file it would have written is not the file it would write now.
pub const PARK_TTL_DAYS: i64 = 7;

// ---------------------------------------------------------------------------
// Payloads
// ---------------------------------------------------------------------------

/// Why this call was parked rather than asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum ParkCause {
    /// Nobody was watching: a routine's run (PLAN 7.3, Phase 16).
    Unattended,
    /// A dialog was open and went unanswered past
    /// [`APPROVAL_TTL`](crate::approval::APPROVAL_TTL).
    Expired,
}

impl ParkCause {
    /// The wire word, shared with the UI.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unattended => "unattended",
            Self::Expired => "expired",
        }
    }
}

/// One parked ask, as the board draws it and `parked_answer` resolves it.
///
/// Everything the dialog had, plus where the run that asked can be picked up
/// again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct ParkedAsk {
    /// UUID v4. What `parked_answer` is called with.
    pub id: String,
    /// The project whose board shows it.
    pub project_id: String,
    /// The session whose run asked, and where an answer resumes it.
    pub session_id: String,
    /// The identity the call was made as.
    pub agent_id: String,
    /// The routine whose run parked, empty for an expired dialog.
    pub routine_id: String,
    /// Its name, so a notification and the board can say it without a lookup.
    pub routine_name: String,
    /// The runbook that was running, when one was.
    pub skill: String,
    /// The turn the call belonged to. Recorded, never resumed into: an answer
    /// opens a new turn.
    pub turn_id: String,
    /// The model's own id for the call, so the audit line of a refusal keys on
    /// the same call as the rest of the run.
    pub call_id: String,
    /// The tool that was asked about.
    pub tool: String,
    /// `<tool>:<args digest>` — what an *allow once* answer matches
    /// ([`fingerprint`](crate::audit::fingerprint)).
    pub fingerprint: String,
    /// Why it was parked.
    pub cause: ParkCause,
    /// The badge. Advisory only (PLAN 3.3).
    pub risk: Risk,
    /// The dialog's title: "Write file", "Run shell command".
    pub title: String,
    /// One line naming the thing.
    pub summary: String,
    /// The structured detail the card draws.
    pub detail: ApprovalDetail,
    /// Why policy was asking.
    pub reason: String,
    /// The grant an *allow standing* answer would sign onto the routine.
    /// `None` is a row no routine can be signed for in advance (PLAN 3.1).
    pub grant: Option<Grant>,
    /// What signing it would cover, in words — on every run of the routine
    /// when there is one, for the rest of the session when there is not.
    pub scope_label: String,
    /// When it was parked, RFC3339 UTC.
    pub parked_at: String,
    /// When it stops being answerable and closes as `blocked`.
    pub expires_at: String,
}

/// What the runtime parks: [`ParkedAsk`] without the fields the store mints.
#[derive(Debug, Clone)]
pub struct ParkDraft<'a> {
    /// The project whose board shows it.
    pub project_id: &'a str,
    /// The session whose run asked.
    pub session_id: &'a str,
    /// The identity the call was made as.
    pub agent_id: &'a str,
    /// The routine whose run parked; empty for an expired dialog.
    pub routine_id: &'a str,
    /// Its name.
    pub routine_name: &'a str,
    /// The runbook that was running.
    pub skill: &'a str,
    /// The turn.
    pub turn_id: &'a str,
    /// The model's own id for the call.
    pub call_id: &'a str,
    /// `<tool>:<args digest>`.
    pub fingerprint: String,
    /// Why it was parked.
    pub cause: ParkCause,
    /// What the dialog would have asked.
    pub request: &'a crate::policy::AskRequest,
}

// ---------------------------------------------------------------------------
// The document
// ---------------------------------------------------------------------------

/// The document itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ParkedFile {
    version: u32,
    parked: Vec<ParkedAsk>,
}

/// The parked-ask store: one mutex over the list, written out on every
/// mutation, like every other store here.
#[derive(Debug)]
pub struct ParkedStore {
    path: PathBuf,
    parked: Mutex<Vec<ParkedAsk>>,
}

impl ParkedStore {
    /// Loads the store from `data_dir`. Never fails: a damaged document starts
    /// empty.
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(PARKED_FILE);

        let parked = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<ParkedFile>(strip_bom(&bytes)) {
                Ok(file) if file.version == SCHEMA_VERSION => {
                    tracing::info!(count = file.parked.len(), "parked asks loaded");
                    file.parked
                }
                Ok(file) => {
                    tracing::error!(
                        found = file.version,
                        expected = SCHEMA_VERSION,
                        "unknown parked store version"
                    );
                    quarantine(&path);
                    Vec::new()
                }
                Err(err) => {
                    tracing::error!(%err, "the parked store is not readable JSON");
                    quarantine(&path);
                    Vec::new()
                }
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(err) => {
                tracing::error!(%err, "could not read the parked store");
                Vec::new()
            }
        };

        Self {
            path,
            parked: Mutex::new(parked),
        }
    }

    /// Locks the list, recovering from a poisoned mutex.
    fn parked(&self) -> MutexGuard<'_, Vec<ParkedAsk>> {
        self.parked
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Records a park, or hands back the one already on file.
    ///
    /// The same call parked twice is one question: within a session, because a
    /// run that repeats a refused call should not fill the board with copies —
    /// and, for a routine, across its runs, because a clock that fires every
    /// five minutes would otherwise ask the same thing all day. What comes
    /// back then is the first run's park, which is the one holding the
    /// half-done work an answer resumes.
    ///
    /// `Err` is the run's park budget ([`MAX_PARKS_PER_RUN`]).
    pub fn park(&self, draft: &ParkDraft<'_>) -> AppResult<ParkedAsk> {
        let mut parked = self.parked();

        let same_run = |ask: &ParkedAsk| {
            if draft.routine_id.is_empty() {
                ask.session_id == draft.session_id
            } else {
                ask.routine_id == draft.routine_id
            }
        };
        if let Some(held) = parked
            .iter()
            .find(|ask| same_run(ask) && ask.fingerprint == draft.fingerprint)
        {
            tracing::debug!(id = %held.id, "this call is already parked");
            return Ok(held.clone());
        }

        let already = parked
            .iter()
            .filter(|ask| ask.session_id == draft.session_id)
            .count();
        if already >= MAX_PARKS_PER_RUN {
            return Err(AppError::ParkBudget {
                most: MAX_PARKS_PER_RUN,
            });
        }

        let at = now();
        let request = draft.request;
        let ask = ParkedAsk {
            id: Uuid::new_v4().to_string(),
            project_id: draft.project_id.to_owned(),
            session_id: draft.session_id.to_owned(),
            agent_id: draft.agent_id.to_owned(),
            routine_id: draft.routine_id.to_owned(),
            routine_name: draft.routine_name.to_owned(),
            skill: draft.skill.to_owned(),
            turn_id: draft.turn_id.to_owned(),
            call_id: draft.call_id.to_owned(),
            tool: request.tool.clone(),
            fingerprint: draft.fingerprint.clone(),
            cause: draft.cause,
            risk: request.risk,
            title: request.title.to_owned(),
            summary: request.summary.clone(),
            detail: request.detail.clone(),
            reason: request.reason.clone(),
            scope_label: match (&request.grant, draft.routine_id.is_empty()) {
                // Signed onto a routine, this outlives every one of its runs,
                // and the card must not offer it in a session's words.
                (Some(grant), false) => grant.standing_label(),
                _ => request.scope_label.clone(),
            },
            grant: request.grant.clone(),
            expires_at: closes_at(&at),
            parked_at: at,
        };

        parked.push(ask.clone());
        self.save(&parked)?;

        tracing::info!(
            id = %ask.id,
            tool = %ask.tool,
            session_id = %ask.session_id,
            cause = ask.cause.as_str(),
            "a call was parked"
        );
        Ok(ask)
    }

    /// Everything still open, oldest first; one project's when named.
    pub fn list(&self, project_id: Option<&str>) -> Vec<ParkedAsk> {
        let mut open: Vec<ParkedAsk> = self
            .parked()
            .iter()
            .filter(|ask| project_id.is_none_or(|wanted| ask.project_id == wanted))
            .cloned()
            .collect();

        open.sort_by(|left, right| {
            left.parked_at
                .cmp(&right.parked_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        open
    }

    /// One parked ask by id.
    pub fn get(&self, id: &str) -> AppResult<ParkedAsk> {
        self.parked()
            .iter()
            .find(|ask| ask.id == id)
            .cloned()
            .ok_or_else(|| AppError::ParkedNotFound { id: id.to_owned() })
    }

    /// Takes one off the list, whoever answered it. `None` when it was already
    /// answered or had expired — the caller then does nothing rather than
    /// resuming a run twice.
    pub fn take(&self, id: &str) -> Option<ParkedAsk> {
        let mut parked = self.parked();
        let at = parked.iter().position(|ask| ask.id == id)?;
        let ask = parked.remove(at);

        if let Err(err) = self.save(&parked) {
            tracing::warn!(%err, id, "a parked ask was answered but the store was not written");
        }
        Some(ask)
    }

    /// Drops everything parked in a session, and says whether it dropped any.
    /// Called when the session goes.
    pub fn forget_session(&self, session_id: &str) -> bool {
        self.retain(|ask| ask.session_id != session_id, "a closed session")
    }

    /// Drops everything parked in a project. Called when the project is
    /// forgotten.
    pub fn forget_project(&self, project_id: &str) -> bool {
        self.retain(|ask| ask.project_id != project_id, "a forgotten project")
    }

    /// Removes the parks nobody answered in time and hands them back, so the
    /// caller can close their runs as `blocked` (PLAN 7.22).
    pub fn prune(&self, now: DateTime<Utc>) -> Vec<ParkedAsk> {
        let mut parked = self.parked();
        let stamp = now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

        let (expired, open): (Vec<ParkedAsk>, Vec<ParkedAsk>) = parked
            .drain(..)
            .partition(|ask| ask.expires_at.as_str() <= stamp.as_str());
        *parked = open;

        if expired.is_empty() {
            return expired;
        }

        if let Err(err) = self.save(&parked) {
            tracing::warn!(%err, "expired parks could not be written out");
        }
        tracing::info!(count = expired.len(), "parked asks expired unanswered");
        expired
    }

    /// Keeps what `keep` says and writes the list out when anything went.
    fn retain(&self, keep: impl Fn(&ParkedAsk) -> bool, why: &str) -> bool {
        let mut parked = self.parked();

        let before = parked.len();
        parked.retain(keep);
        let dropped = before - parked.len();
        if dropped == 0 {
            return false;
        }

        if let Err(err) = self.save(&parked) {
            tracing::warn!(%err, "parked asks were dropped but the store was not written");
        }
        tracing::info!(dropped, why, "parked asks dropped");
        true
    }

    /// Serializes the list and replaces the document atomically.
    fn save(&self, parked: &[ParkedAsk]) -> AppResult<()> {
        let file = ParkedFile {
            version: SCHEMA_VERSION,
            parked: parked.to_vec(),
        };
        let bytes = serde_json::to_vec_pretty(&file).map_err(|err| AppError::Store {
            action: "serialize",
            source: io::Error::other(err),
        })?;

        write_atomic(&self.path, &bytes).map_err(|err| {
            tracing::error!(%err, path = %self.path.display(), "could not write the parked store");
            AppError::Store {
                action: "save",
                source: err,
            }
        })
    }

    /// Where the document lives.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// [`PARK_TTL_DAYS`] after `at`, in the same fixed-width form, so the two
/// compare as strings. A stamp that will not parse closes immediately rather
/// than never.
fn closes_at(at: &str) -> String {
    DateTime::parse_from_rfc3339(at)
        .map(|born| born.with_timezone(&Utc) + TimeDelta::days(PARK_TTL_DAYS))
        .unwrap_or_else(|_| Utc::now())
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests;
