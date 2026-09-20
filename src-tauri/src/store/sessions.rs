//! The session document: `sessions.json`.
//!
//! A session is a conversation in a project: title, transcript, timestamps.
//! Whether a turn is running is never persisted — [`SessionState`] is derived
//! from the turn registry on read — so a session interrupted by a crash comes
//! back idle.
//!
//! A [`Compaction`] (Phase 14) is a pointer plus derived state, never a
//! deletion: the transcript stays whole, and only what [`compact::tail`] hands
//! the model changes.
//!
//! Each finished turn's [`TurnCost`] (Phase 17) is stored here rather than on
//! the audit line: tokens are spent per model round, and a turn that called no
//! tool still spends them. A run's cost joins the two on `turn_id`
//! ([`board::trace`](crate::board::trace)).
//!
//! One document holds every session; [`SessionStore::save`] is the seam if a
//! transcript ever needs a file of its own.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::approval::ApprovalRequest;
use crate::compact::{self, Plan};

use super::agents::DEFAULT_AGENT_ID;
use super::{now, quarantine, strip_bom, write_atomic};
use crate::error::{AppError, AppResult};

mod payload;

pub use payload::*;

/// Name of the document under the application-data directory.
const SESSIONS_FILE: &str = "sessions.json";

/// Schema version of [`SessionsFile`], independent of the project document's.
const SCHEMA_VERSION: u32 = 1;

/// Title given to a session created without one.
const DEFAULT_TITLE: &str = "New session";

/// How much of the first user message becomes the session title.
const TITLE_MAX_CHARS: usize = 48;

// ---------------------------------------------------------------------------
// On-disk shapes
// ---------------------------------------------------------------------------

/// The document itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionsFile {
    version: u32,
    sessions: Vec<StoredSession>,
}

/// A session record as persisted. Not [`SessionSummary`], so derived fields
/// cannot be persisted by accident. Every later field is `#[serde(default)]`,
/// which is how older documents keep loading.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredSession {
    id: String,
    project_id: String,
    /// The bound identity; `None` before Phase 12, resolving to the built-in
    /// one.
    #[serde(default)]
    agent_id: Option<String>,
    /// The override of the identity's provider row and model (PLAN 7.19).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    title: String,
    created_at: String,
    updated_at: String,
    #[serde(default)]
    messages: Vec<Message>,
    /// What has been folded, if anything.
    #[serde(default)]
    compaction: Option<Compaction>,
    /// The brief that opened this session, if one did.
    #[serde(default)]
    delegated: Option<Delegated>,
    /// The routine that opened this session, if one did.
    #[serde(default)]
    scheduled: Option<Scheduled>,
    /// What each finished turn spent, oldest first. Per turn, because a run
    /// covers only some of a session's turns.
    #[serde(default)]
    costs: Vec<TurnCost>,
}

impl StoredSession {
    /// The row the sidebar draws, at the state the caller measured.
    fn to_summary(&self, state: SessionState) -> SessionSummary {
        SessionSummary {
            id: self.id.clone(),
            project_id: self.project_id.clone(),
            agent_id: self
                .agent_id
                .clone()
                .unwrap_or_else(|| DEFAULT_AGENT_ID.to_owned()),
            provider_id: self.provider_id.clone(),
            model: self.model.clone(),
            title: self.title.clone(),
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
            // A transcript that overflows `u32` is not a transcript.
            message_count: u32::try_from(self.messages.len()).unwrap_or(u32::MAX),
            state,
            delegated: self.delegated.clone(),
            scheduled: self.scheduled.clone(),
            cost: Cost::of(&self.costs),
        }
    }
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// The session store: one mutex over the list, written out on every mutation.
/// Methods that produce a [`SessionSummary`] take its state from the caller,
/// since the store cannot see the turn registry.
#[derive(Debug)]
pub struct SessionStore {
    path: PathBuf,
    sessions: Mutex<Vec<StoredSession>>,
}

impl SessionStore {
    /// Loads the store from `data_dir`. Never fails: an unreadable document
    /// starts empty.
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(SESSIONS_FILE);

        let sessions = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<SessionsFile>(strip_bom(&bytes)) {
                Ok(file) if file.version == SCHEMA_VERSION => {
                    tracing::info!(count = file.sessions.len(), "session store loaded");
                    file.sessions
                }
                Ok(file) => {
                    tracing::error!(
                        found = file.version,
                        expected = SCHEMA_VERSION,
                        "unknown session store version"
                    );
                    quarantine(&path);
                    Vec::new()
                }
                Err(err) => {
                    tracing::error!(%err, "session store is not readable JSON");
                    quarantine(&path);
                    Vec::new()
                }
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                tracing::info!("no session store yet; starting empty");
                Vec::new()
            }
            Err(err) => {
                tracing::error!(%err, "could not read the session store");
                Vec::new()
            }
        };

        Self {
            path,
            sessions: Mutex::new(sessions),
        }
    }

    /// Locks the list, recovering from poison: it cannot be left torn.
    fn sessions(&self) -> MutexGuard<'_, Vec<StoredSession>> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Creates a session in `project_id`, bound to `agent_id` for good: a
    /// transcript is the record of what that identity did. An empty title
    /// becomes [`DEFAULT_TITLE`] until the first user message
    /// ([`SessionStore::append`]). The caller checks the identity exists
    /// ([`AppState::create_session`](crate::AppState::create_session)).
    pub fn create(
        &self,
        project_id: &str,
        title: Option<&str>,
        agent_id: &str,
    ) -> AppResult<SessionSummary> {
        self.create_bound(project_id, title, agent_id, None, None)
    }

    /// [`SessionStore::create`], with an override of the identity's provider
    /// and model written in the same save (PLAN 7.19).
    pub fn create_bound(
        &self,
        project_id: &str,
        title: Option<&str>,
        agent_id: &str,
        provider_id: Option<&str>,
        model: Option<&str>,
    ) -> AppResult<SessionSummary> {
        let mut sessions = self.sessions();
        let mut session = Self::record(project_id, title, agent_id, None, None);
        session.provider_id = provider_id.map(str::to_owned);
        session.model = model.map(str::to_owned);
        Self::push(&mut sessions, session, |all| self.save(all))
    }

    /// Creates the session a brief opens (Phase 15): an ordinary session with a
    /// [`Delegated`] on it.
    pub fn create_delegated(
        &self,
        project_id: &str,
        title: Option<&str>,
        agent_id: &str,
        delegated: Delegated,
    ) -> AppResult<SessionSummary> {
        self.open_session(project_id, title, agent_id, Some(delegated), None)
    }

    /// Creates the session a routine opens (Phase 16), with a [`Scheduled`] on
    /// it.
    pub fn create_scheduled(
        &self,
        project_id: &str,
        title: Option<&str>,
        agent_id: &str,
        scheduled: Scheduled,
    ) -> AppResult<SessionSummary> {
        self.open_session(project_id, title, agent_id, None, Some(scheduled))
    }

    /// The one place a session record is built.
    fn open_session(
        &self,
        project_id: &str,
        title: Option<&str>,
        agent_id: &str,
        delegated: Option<Delegated>,
        scheduled: Option<Scheduled>,
    ) -> AppResult<SessionSummary> {
        let mut sessions = self.sessions();
        let session = Self::record(project_id, title, agent_id, delegated, scheduled);
        Self::push(&mut sessions, session, |all| self.save(all))
    }

    /// A fresh record, inheriting its identity's provider and model.
    fn record(
        project_id: &str,
        title: Option<&str>,
        agent_id: &str,
        delegated: Option<Delegated>,
        scheduled: Option<Scheduled>,
    ) -> StoredSession {
        let stamp = now();
        StoredSession {
            id: Uuid::new_v4().to_string(),
            project_id: project_id.to_owned(),
            agent_id: Some(agent_id.to_owned()),
            provider_id: None,
            model: None,
            title: match title.map(str::trim) {
                Some("") | None => DEFAULT_TITLE.to_owned(),
                Some(given) => given.to_owned(),
            },
            created_at: stamp.clone(),
            updated_at: stamp,
            messages: Vec::new(),
            compaction: None,
            delegated,
            scheduled,
            costs: Vec::new(),
        }
    }

    /// Appends a record and saves the list.
    fn push(
        sessions: &mut Vec<StoredSession>,
        session: StoredSession,
        save: impl FnOnce(&[StoredSession]) -> AppResult<()>,
    ) -> AppResult<SessionSummary> {
        let created = session.to_summary(SessionState::Idle);
        sessions.push(session);
        save(sessions)?;

        tracing::info!(
            id = %created.id,
            project_id = %created.project_id,
            agent_id = %created.agent_id,
            "session created"
        );
        Ok(created)
    }

    /// A project's sessions, most recently active first, stamped by `state_of`.
    /// Fixed-width UTC RFC3339 strings sort as instants.
    pub fn list(
        &self,
        project_id: &str,
        state_of: &dyn Fn(&str) -> SessionState,
    ) -> Vec<SessionSummary> {
        let sessions = self.sessions();

        let mut out: Vec<SessionSummary> = sessions
            .iter()
            .filter(|s| s.project_id == project_id)
            .map(|s| s.to_summary(state_of(&s.id)))
            .collect();
        out.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| b.created_at.cmp(&a.created_at))
        });
        out
    }

    /// One session with its transcript.
    pub fn open(&self, id: &str, state: SessionState) -> AppResult<SessionDetail> {
        let sessions = self.sessions();
        let session = Self::find(&sessions, id)?;

        Ok(SessionDetail {
            session: session.to_summary(state),
            messages: session.messages.clone(),
            compaction: session.compaction.clone(),
            // Composed in by `AppState`, which can see the approval registry.
            pending_approvals: Vec::new(),
        })
    }

    /// The session as one row, without its transcript.
    pub fn summary(&self, id: &str, state: SessionState) -> AppResult<SessionSummary> {
        let sessions = self.sessions();
        Ok(Self::find(&sessions, id)?.to_summary(state))
    }

    /// The transcript alone, for building a model request.
    pub fn messages(&self, id: &str) -> AppResult<Vec<Message>> {
        let sessions = self.sessions();
        Ok(Self::find(&sessions, id)?.messages.clone())
    }

    /// The transcript and its compaction (Phase 14), under one lock so they
    /// match.
    pub fn context(&self, id: &str) -> AppResult<(Vec<Message>, Option<Compaction>)> {
        let sessions = self.sessions();
        let session = Self::find(&sessions, id)?;
        Ok((session.messages.clone(), session.compaction.clone()))
    }

    /// Folds the older part of a transcript, returning the new compaction or
    /// `None` when nothing moved (too few turns, or the same cut as before).
    /// `force` is the button; otherwise the transcript must pass
    /// [`compact::COMPACT_AT_BYTES`]. The state is derived from the messages
    /// every time, never from a previous fold. `updated_at` is left alone.
    pub fn compact(&self, id: &str, force: bool) -> AppResult<Option<Compaction>> {
        let mut sessions = self.sessions();
        let session = Self::find_mut(&mut sessions, id)?;

        let Some(Plan {
            through_message_id,
            folded,
            state,
        }) = compact::plan(&session.messages, force)
        else {
            return Ok(None);
        };

        if session
            .compaction
            .as_ref()
            .is_some_and(|held| held.through_message_id == through_message_id)
        {
            tracing::debug!(session_id = id, "nothing new to fold");
            return Ok(None);
        }

        let compaction = Compaction {
            through_message_id,
            folded,
            state,
            at: now(),
        };
        session.compaction = Some(compaction.clone());

        self.save(&sessions)?;
        tracing::info!(session_id = id, folded, force, "session compacted");
        Ok(Some(compaction))
    }

    /// Records what a turn spent (Phase 17), for every ending. Charging the same
    /// turn again replaces the figure. `updated_at` is left alone.
    pub fn charge(&self, id: &str, cost: TurnCost) -> AppResult<Cost> {
        let mut sessions = self.sessions();
        let session = Self::find_mut(&mut sessions, id)?;

        match session
            .costs
            .iter_mut()
            .find(|held| held.turn_id == cost.turn_id)
        {
            Some(held) => *held = cost,
            None => session.costs.push(cost),
        }
        let total = Cost::of(&session.costs);

        self.save(&sessions)?;
        Ok(total)
    }

    /// Each turn's cost, oldest first, joined to runs by
    /// [`board::trace`](crate::board::trace).
    pub fn costs(&self, id: &str) -> AppResult<Vec<TurnCost>> {
        let sessions = self.sessions();
        Ok(Self::find(&sessions, id)?.costs.clone())
    }

    /// Which project a session belongs to.
    pub fn project_of(&self, id: &str) -> AppResult<String> {
        let sessions = self.sessions();
        Ok(Self::find(&sessions, id)?.project_id.clone())
    }

    /// Which identity a session is bound to, as it was stored.
    ///
    /// `None` is a session written before identities existed, which is not the
    /// same fact as one that chose the default and is deliberately not
    /// flattened into it here — resolving that is
    /// [`AgentStore::resolve`](super::agents::AgentStore::resolve)'s job.
    pub fn agent_of(&self, id: &str) -> AppResult<Option<String>> {
        let sessions = self.sessions();
        Ok(Self::find(&sessions, id)?.agent_id.clone())
    }

    /// A session's override of its identity's provider and model, as stored.
    pub fn binding_of(&self, id: &str) -> AppResult<(Option<String>, Option<String>)> {
        let sessions = self.sessions();
        let session = Self::find(&sessions, id)?;
        Ok((session.provider_id.clone(), session.model.clone()))
    }

    /// Writes or clears a session's override (PLAN 7.19). The caller checks
    /// the row exists and that no turn runs
    /// ([`AppState::set_session_binding`](crate::AppState::set_session_binding)).
    /// Does not bump `updated_at`: nothing was said.
    pub fn set_binding(
        &self,
        id: &str,
        provider_id: Option<&str>,
        model: Option<&str>,
        state: SessionState,
    ) -> AppResult<SessionSummary> {
        let mut sessions = self.sessions();
        let session = Self::find_mut(&mut sessions, id)?;
        session.provider_id = provider_id.map(str::to_owned);
        session.model = model.map(str::to_owned);
        let bound = session.to_summary(state);

        self.save(&sessions)?;
        tracing::info!(id, ?provider_id, ?model, "session binding changed");
        Ok(bound)
    }

    /// How many sessions override to `provider_id`.
    pub fn count_for_provider(&self, provider_id: &str) -> usize {
        self.sessions()
            .iter()
            .filter(|session| session.provider_id.as_deref() == Some(provider_id))
            .count()
    }

    /// How many sessions run as `agent_id`.
    ///
    /// Asked before an identity is deleted. Sessions that named nothing are not
    /// counted against the built-in identity, and do not need to be: the
    /// built-in one cannot be deleted at all.
    pub fn count_for_agent(&self, agent_id: &str) -> usize {
        let sessions = self.sessions();
        sessions
            .iter()
            .filter(|session| session.agent_id.as_deref() == Some(agent_id))
            .count()
    }

    /// Renames a session. An empty title is refused rather than stored.
    pub fn rename(&self, id: &str, title: &str, state: SessionState) -> AppResult<SessionSummary> {
        let title = title.trim();
        if title.is_empty() {
            return Err(AppError::SessionTitle);
        }

        let mut sessions = self.sessions();
        let session = Self::find_mut(&mut sessions, id)?;
        session.title = title.to_owned();
        session.updated_at = now();
        let renamed = session.to_summary(state);

        self.save(&sessions)?;
        tracing::debug!(id, "session renamed");
        Ok(renamed)
    }

    /// Appends a message, and returns the session as it now stands.
    ///
    /// The first user message also names the session, when nothing else has:
    /// a sidebar of six rows all reading "New session" cannot be navigated,
    /// and asking a user to name a conversation before they have had it asks
    /// too early.
    pub fn append(
        &self,
        id: &str,
        message: Message,
        state: SessionState,
    ) -> AppResult<SessionSummary> {
        let mut sessions = self.sessions();
        let session = Self::find_mut(&mut sessions, id)?;

        if session.title == DEFAULT_TITLE && message.role == Role::User {
            session.title = title_from(&message.text);
        }
        session.messages.push(message);
        session.updated_at = now();
        let updated = session.to_summary(state);

        self.save(&sessions)?;
        Ok(updated)
    }

    /// Updates one tool call by `call_id`, returning whether it was found. A
    /// `None` summary or image keeps what an earlier update recorded.
    pub fn set_tool_call_status(
        &self,
        id: &str,
        call_id: &str,
        status: ToolCallStatus,
        summary: Option<String>,
        image_path: Option<String>,
    ) -> AppResult<bool> {
        let mut sessions = self.sessions();
        let session = Self::find_mut(&mut sessions, id)?;

        let found = session
            .messages
            .iter_mut()
            .flat_map(|message| message.tool_calls.iter_mut())
            .find(|call| call.call_id == call_id)
            .map(|call| {
                call.status = status;
                if summary.is_some() {
                    call.summary = summary;
                }
                if image_path.is_some() {
                    call.image_path = image_path;
                }
            })
            .is_some();

        if found {
            self.save(&sessions)?;
        } else {
            tracing::warn!(
                session_id = id,
                call_id,
                "no such tool call in the transcript"
            );
        }
        Ok(found)
    }

    /// Deletes a session and its transcript.
    pub fn delete(&self, id: &str) -> AppResult<()> {
        let mut sessions = self.sessions();

        let before = sessions.len();
        sessions.retain(|s| s.id != id);
        if sessions.len() == before {
            return Err(AppError::SessionNotFound { id: id.to_owned() });
        }

        self.save(&sessions)?;
        tracing::info!(id, "session deleted");
        Ok(())
    }

    /// Deletes every session of a forgotten project, returning how many went.
    pub fn delete_for_project(&self, project_id: &str) -> AppResult<usize> {
        let mut sessions = self.sessions();

        let before = sessions.len();
        sessions.retain(|s| s.project_id != project_id);
        let removed = before - sessions.len();

        if removed > 0 {
            self.save(&sessions)?;
            tracing::info!(project_id, removed, "sessions deleted with their project");
        }
        Ok(removed)
    }

    /// Looks a session up, or reports that the caller's list is stale.
    fn find<'a>(sessions: &'a [StoredSession], id: &str) -> AppResult<&'a StoredSession> {
        sessions
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| AppError::SessionNotFound { id: id.to_owned() })
    }

    /// [`SessionStore::find`], mutably.
    fn find_mut<'a>(
        sessions: &'a mut [StoredSession],
        id: &str,
    ) -> AppResult<&'a mut StoredSession> {
        sessions
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or_else(|| AppError::SessionNotFound { id: id.to_owned() })
    }

    /// Serializes the list and replaces the document atomically.
    fn save(&self, sessions: &[StoredSession]) -> AppResult<()> {
        let file = SessionsFile {
            version: SCHEMA_VERSION,
            sessions: sessions.to_vec(),
        };

        let bytes = serde_json::to_vec_pretty(&file).map_err(|err| {
            tracing::error!(%err, "could not serialize the session store");
            AppError::Store {
                action: "serialize",
                source: io::Error::other(err),
            }
        })?;

        write_atomic(&self.path, &bytes).map_err(|err| {
            tracing::error!(%err, path = %self.path.display(), "could not save the session store");
            AppError::Store {
                action: "save",
                source: err,
            }
        })
    }
}

/// A session title from the first user message: whitespace collapsed, cut at
/// the last word boundary in the final quarter of the budget.
fn title_from(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return DEFAULT_TITLE.to_owned();
    }

    // By character, so a multi-byte character is never split.
    let Some(end) = flat.char_indices().map(|(i, _)| i).nth(TITLE_MAX_CHARS) else {
        return flat;
    };

    let head = &flat[..end];
    let keep = head
        .rfind(' ')
        .filter(|space| space * 4 >= end * 3)
        .unwrap_or(end);

    format!("{}…", head[..keep].trim_end())
}

#[cfg(test)]
mod tests;
