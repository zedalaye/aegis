//! The session document: `sessions.json`.
//!
//! A session is a conversation inside a project: a title, a transcript, and
//! the timestamps that order it in the sidebar. Everything the UI shows about
//! a session that survives a restart is here; everything that does not — most
//! importantly whether a turn is running — is deliberately absent.
//!
//! That absence is the one design decision worth stating up front. A session's
//! [`SessionState`] is derived on read from the live turn registry, never
//! persisted, for the same reason `workspace_exists` is never persisted: if
//! Aegis is killed mid-turn, a stored `running` would come back after the
//! restart and there would be nothing left to finish it. A session that was
//! interrupted is idle, because that is what it is.
//!
//! One document holds every session of every project. At MVP scale — a handful
//! of projects, transcripts a human typed — that is a file a person can open
//! and read, which is worth more than the write amplification it costs. A
//! transcript that outgrows it wants its own file; the seam for that is
//! [`SessionStore::save`], the only place the whole document is serialized.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::approval::ApprovalRequest;

use super::{now, quarantine, strip_bom, write_atomic};
use crate::error::{AppError, AppResult};

/// Name of the document under the application-data directory.
const SESSIONS_FILE: &str = "sessions.json";

/// Schema version of [`SessionsFile`].
///
/// Independent of the project document's version: the two files change at very
/// different rates, and a migration to one has no business quarantining the
/// other.
const SCHEMA_VERSION: u32 = 1;

/// Title given to a session created without one.
const DEFAULT_TITLE: &str = "New session";

/// How much of the first user message becomes the session title.
///
/// Long enough to tell two sessions apart in a sidebar, short enough not to
/// wrap.
const TITLE_MAX_CHARS: usize = 48;

// ---------------------------------------------------------------------------
// IPC payloads (PLAN 2.1, "Sessions and turns")
// ---------------------------------------------------------------------------

/// Lifecycle of a session.
///
/// Derived from the turn registry on every read, never stored — see the module
/// documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum SessionState {
    /// Nothing is running. The composer is open.
    #[default]
    Idle,
    /// A turn is streaming.
    Running,
    /// A turn is blocked on an approval (Phase 6).
    AwaitingApproval,
    /// The last turn ended with an error. Cleared by the next send.
    Error,
}

/// Who produced a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum Role {
    /// Typed by the person.
    User,
    /// Produced by the model.
    Assistant,
    /// A tool result, answering one of the assistant's calls.
    Tool,
    /// The instructions the runtime prepends.
    ///
    /// Never persisted in a transcript: it is rebuilt for every request,
    /// because the workspace path it names can change between turns.
    System,
}

/// How far one tool call got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum ToolCallStatus {
    /// Assembled, not yet judged by policy.
    Pending,
    /// Cleared to run.
    Approved,
    /// Refused, by policy or by the user.
    Denied,
    /// Executing.
    Running,
    /// Finished, and did what was asked.
    Ok,
    /// Finished, and did not.
    Error,
    /// Abandoned because the turn was cancelled.
    Cancelled,
}

/// One tool call, as the transcript records it.
///
/// `args_json` is what the model actually sent, kept verbatim so the card in
/// the UI shows the call that was made rather than the call policy resolved.
/// `summary` is the one human line — never a raw blob, because a transcript
/// that inlines 200 KB of file content is a transcript nobody scrolls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct ToolCallRecord {
    /// The model's own id for the call.
    pub call_id: String,
    /// Tool name: `fs_list`, `fs_read`, `fs_write`, ...
    pub tool: String,
    /// The arguments as the model sent them, as a JSON string.
    pub args_json: String,
    /// How far it got.
    pub status: ToolCallStatus,
    /// One human line naming the result, or `null` before there is one.
    pub summary: Option<String>,
    /// A local image the call produced, for the transcript to show.
    ///
    /// Only `screen_capture` sets it, and it is a path rather than the bytes
    /// (PLAN 5.4): the WebView loads it through the asset protocol, which is
    /// scoped to the capture directory. Persisted, so re-opening a session
    /// shows the capture again instead of a line saying one was taken.
    ///
    /// `#[serde(default)]` for the transcripts written before this field
    /// existed — a session on disk must keep opening.
    #[serde(default)]
    pub image_path: Option<String>,
}

/// One message in a transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct Message {
    /// UUID v4.
    pub id: String,
    /// Who produced it.
    pub role: Role,
    /// The text. Empty for an assistant message that only made tool calls.
    pub text: String,
    /// The calls this message made. Assistant messages only.
    #[serde(default)]
    pub tool_calls: Vec<ToolCallRecord>,
    /// Which call this message answers. `tool` messages only.
    #[serde(default)]
    pub tool_call_id: Option<String>,
    /// RFC3339, UTC.
    pub created_at: String,
}

impl Message {
    /// A message from the person.
    pub fn user(text: impl Into<String>) -> Self {
        Self::new(Role::User, text.into())
    }

    /// A message from the model, with whatever calls it made.
    pub fn assistant(text: impl Into<String>, tool_calls: Vec<ToolCallRecord>) -> Self {
        Self {
            tool_calls,
            ..Self::new(Role::Assistant, text.into())
        }
    }

    /// A tool result answering `call_id`. The text is a `ToolResult` envelope.
    pub fn tool(call_id: impl Into<String>, envelope: impl Into<String>) -> Self {
        Self {
            tool_call_id: Some(call_id.into()),
            ..Self::new(Role::Tool, envelope.into())
        }
    }

    /// The shared skeleton.
    fn new(role: Role, text: String) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            role,
            text,
            tool_calls: Vec::new(),
            tool_call_id: None,
            created_at: now(),
        }
    }

    /// Whether this message carries nothing at all.
    ///
    /// A turn that produced no text and made no calls has nothing to record,
    /// and an empty bubble in the transcript is worse than no bubble.
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.tool_calls.is_empty()
    }
}

/// One row of a project's session list.
///
/// `state` is live and `message_count` is derived; neither is stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct SessionSummary {
    /// UUID v4.
    pub id: String,
    /// The project this session belongs to.
    pub project_id: String,
    /// Display title. Taken from the first user message when not given.
    pub title: String,
    /// RFC3339, UTC.
    pub created_at: String,
    /// RFC3339, UTC. Bumped by every message and every rename.
    pub updated_at: String,
    /// How many messages the transcript holds.
    pub message_count: u32,
    /// What the session is doing *right now*.
    pub state: SessionState,
}

/// A session and its transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct SessionDetail {
    /// The session itself.
    pub session: SessionSummary,
    /// Oldest first — the order the transcript is read in.
    pub messages: Vec<Message>,
    /// Approvals this session is blocked on.
    ///
    /// Filled by [`AppState::session_detail`](crate::AppState::session_detail)
    /// rather than here: the transcript is on disk, and what a session is
    /// waiting for is a fact about this process. It is on the detail at all so
    /// that a window reopened mid-turn re-draws the dialog it missed, instead
    /// of leaving a turn blocked on a prompt nobody can see.
    pub pending_approvals: Vec<ApprovalRequest>,
}

/// What `session_send` hands back (PLAN 2.1).
///
/// The turn itself is reported through events; this is only the handle needed
/// to cancel it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct TurnHandle {
    /// The session the turn runs in.
    pub session_id: String,
    /// The turn, unique within the process.
    pub turn_id: String,
}

// ---------------------------------------------------------------------------
// On-disk shapes
// ---------------------------------------------------------------------------

/// The document itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionsFile {
    version: u32,
    sessions: Vec<StoredSession>,
}

/// A session record as persisted.
///
/// Deliberately not [`SessionSummary`]: `state` and `message_count` are both
/// derived, and keeping the two types apart makes it impossible to persist
/// either by accident.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredSession {
    id: String,
    project_id: String,
    title: String,
    created_at: String,
    updated_at: String,
    #[serde(default)]
    messages: Vec<Message>,
}

impl StoredSession {
    /// The row the sidebar draws, at the state the caller measured.
    fn to_summary(&self, state: SessionState) -> SessionSummary {
        SessionSummary {
            id: self.id.clone(),
            project_id: self.project_id.clone(),
            title: self.title.clone(),
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
            // A transcript that overflows `u32` is not a transcript.
            message_count: u32::try_from(self.messages.len()).unwrap_or(u32::MAX),
            state,
        }
    }
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// The session store: the in-memory sessions plus the document backing them.
///
/// Same shape as [`Store`](super::projects::Store), and for the same reasons:
/// one mutex over the whole list, written out on every mutation, so "what is
/// on disk" always equals "what is in memory" once a call returns.
///
/// Every method that produces a [`SessionSummary`] is *given* the state to
/// stamp on it rather than inventing one. The store cannot see the turn
/// registry, and a default of `Idle` invented here is exactly the lie that
/// would draw a running session as idle.
#[derive(Debug)]
pub struct SessionStore {
    path: PathBuf,
    sessions: Mutex<Vec<StoredSession>>,
}

impl SessionStore {
    /// Loads the store from `data_dir`.
    ///
    /// Never fails, for the reason [`Store::load`](super::projects::Store::load)
    /// does not: a tray app that will not boot cannot explain why it did not.
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

    /// Locks the list, recovering from a poisoned mutex.
    ///
    /// Same reasoning as the project store: the guarded value is a `Vec` that
    /// is only ever replaced wholesale, so it cannot be torn, and propagating
    /// a panic through every later command is strictly worse.
    fn sessions(&self) -> MutexGuard<'_, Vec<StoredSession>> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Creates a session in `project_id`.
    ///
    /// An empty title becomes [`DEFAULT_TITLE`], which the first user message
    /// then replaces — see [`SessionStore::append`].
    pub fn create(&self, project_id: &str, title: Option<&str>) -> AppResult<SessionSummary> {
        let stamp = now();
        let session = StoredSession {
            id: Uuid::new_v4().to_string(),
            project_id: project_id.to_owned(),
            title: match title.map(str::trim) {
                Some("") | None => DEFAULT_TITLE.to_owned(),
                Some(given) => given.to_owned(),
            },
            created_at: stamp.clone(),
            updated_at: stamp,
            messages: Vec::new(),
        };
        let created = session.to_summary(SessionState::Idle);

        let mut sessions = self.sessions();
        sessions.push(session);
        self.save(&sessions)?;

        tracing::info!(id = %created.id, project_id, "session created");
        Ok(created)
    }

    /// A project's sessions, most recently active first.
    ///
    /// `state_of` is asked for each session's live state; the store has no way
    /// to know it. Timestamps are fixed-width UTC RFC3339, so comparing them as
    /// strings *is* comparing them as instants.
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

    /// Which project a session belongs to.
    pub fn project_of(&self, id: &str) -> AppResult<String> {
        let sessions = self.sessions();
        Ok(Self::find(&sessions, id)?.project_id.clone())
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

    /// Updates one tool call inside whichever assistant message holds it.
    ///
    /// Addressed by `call_id` alone: the id is the model's, unique within the
    /// turn, and scanning for it costs nothing at transcript scale. `summary`
    /// of `None` leaves the existing one alone, so a status change does not
    /// erase the line a finished call already wrote.
    ///
    /// Returns whether a call was found. A caller that silently updated
    /// nothing is a bug worth seeing in the log.
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
                // Same rule as the summary, for the same reason: a later
                // status change carrying nothing must not erase what an
                // earlier one recorded.
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

    /// Deletes every session of a project, and reports how many went.
    ///
    /// Called when a project is forgotten. Sessions of a project that no
    /// longer exists are unreachable, and leaving them behind would grow the
    /// document forever with transcripts nothing can open.
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
    ///
    /// Takes the guard, so the only way to reach it is to already hold the
    /// lock: a caller cannot mutate the list and forget to persist it.
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

/// A session title taken from the first thing the user said.
///
/// Whitespace collapses, so a pasted block does not become a title with a line
/// break in it. The cut prefers the last word boundary in the final quarter of
/// the budget: cutting mid-word reads as a bug, cutting a little short does
/// not.
fn title_from(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return DEFAULT_TITLE.to_owned();
    }

    // `char_indices` rather than byte slicing: a title is user text, and
    // cutting a multi-byte character in half would panic.
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
mod tests {
    use super::*;

    use tempfile::TempDir;

    /// Every session idle — the state a store built in a test is in.
    fn idle(_id: &str) -> SessionState {
        SessionState::Idle
    }

    /// A store plus the directory it lives in.
    struct Fixture {
        _dir: TempDir,
        data: PathBuf,
        store: SessionStore,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = TempDir::new().expect("temp dir");
            let data = dir.path().join("data");
            fs::create_dir_all(&data).expect("data dir");
            let store = SessionStore::load(&data);
            Self {
                _dir: dir,
                data,
                store,
            }
        }

        fn document(&self) -> PathBuf {
            self.data.join(SESSIONS_FILE)
        }

        /// Reloads from disk, as a restart would.
        fn reopen(&self) -> SessionStore {
            SessionStore::load(&self.data)
        }
    }

    #[test]
    fn a_session_survives_a_restart() {
        let fx = Fixture::new();
        let created = fx.store.create("p1", Some("Refactor")).expect("create");
        fx.store
            .append(&created.id, Message::user("hello"), SessionState::Idle)
            .expect("append");

        let reopened = fx.reopen();
        let detail = reopened
            .open(&created.id, SessionState::Idle)
            .expect("open");

        assert_eq!(detail.session.id, created.id);
        assert_eq!(detail.session.title, "Refactor");
        assert_eq!(detail.messages.len(), 1);
        assert_eq!(detail.messages[0].text, "hello");
    }

    /// The one invariant the module exists to protect: a process killed
    /// mid-turn must not come back claiming the turn is still running.
    #[test]
    fn a_restart_never_reports_a_session_as_running() {
        let fx = Fixture::new();
        let created = fx.store.create("p1", None).expect("create");
        fx.store
            .append(&created.id, Message::user("go"), SessionState::Running)
            .expect("append");

        // Whatever state was stamped on the returned summary, nothing about it
        // reached the document.
        let raw = fs::read_to_string(fx.document()).expect("document");
        assert!(
            !raw.contains("running"),
            "session state must never be persisted: {raw}"
        );

        let listed = fx.reopen().list("p1", &idle);
        assert_eq!(listed[0].state, SessionState::Idle);
    }

    #[test]
    fn the_first_user_message_names_an_unnamed_session() {
        let fx = Fixture::new();
        let created = fx.store.create("p1", None).expect("create");
        assert_eq!(created.title, DEFAULT_TITLE);

        let updated = fx
            .store
            .append(
                &created.id,
                Message::user("  Explain the   policy matrix\nplease  "),
                SessionState::Idle,
            )
            .expect("append");

        assert_eq!(updated.title, "Explain the policy matrix please");
    }

    #[test]
    fn a_session_the_user_named_keeps_its_name() {
        let fx = Fixture::new();
        let created = fx.store.create("p1", Some("Mine")).expect("create");

        let updated = fx
            .store
            .append(&created.id, Message::user("anything"), SessionState::Idle)
            .expect("append");

        assert_eq!(updated.title, "Mine");
    }

    #[test]
    fn a_long_first_message_is_cut_on_a_word_boundary() {
        let title = title_from(
            "Please read every file under src and tell me which of them still \
             mention the old name",
        );

        assert!(title.ends_with('…'), "{title}");
        assert!(title.chars().count() <= TITLE_MAX_CHARS + 1, "{title}");
        assert!(
            !title.trim_end_matches('…').ends_with(' '),
            "the ellipsis follows a word, not a space: {title}"
        );
        assert!(
            title.starts_with("Please read every file under src"),
            "{title}"
        );
    }

    /// A word longer than the budget has no boundary to fall back on; cutting
    /// it mid-word is the only option, and must not panic on a multi-byte
    /// character.
    #[test]
    fn a_title_never_splits_a_character() {
        let title = title_from(&"é".repeat(200));
        assert_eq!(title.chars().count(), TITLE_MAX_CHARS + 1);
    }

    #[test]
    fn sessions_list_most_recently_active_first() {
        let fx = Fixture::new();
        let first = fx.store.create("p1", Some("First")).expect("create");
        let second = fx.store.create("p1", Some("Second")).expect("create");
        let other = fx.store.create("p2", Some("Elsewhere")).expect("create");

        // Touching the older session moves it to the top.
        fx.store
            .append(&first.id, Message::user("later"), SessionState::Idle)
            .expect("append");

        let listed = fx.store.list("p1", &idle);
        let titles: Vec<&str> = listed.iter().map(|s| s.title.as_str()).collect();

        assert_eq!(titles, vec!["First", "Second"]);
        assert!(
            !listed.iter().any(|s| s.id == other.id),
            "another project's sessions are not this project's"
        );
        assert_eq!(listed[0].message_count, 1);
        assert_eq!(listed[1].id, second.id);
    }

    #[test]
    fn the_live_state_comes_from_the_caller() {
        let fx = Fixture::new();
        let created = fx.store.create("p1", None).expect("create");
        let id = created.id.clone();

        let running = |candidate: &str| {
            if candidate == id {
                SessionState::Running
            } else {
                SessionState::Idle
            }
        };

        assert_eq!(
            fx.store.list("p1", &running)[0].state,
            SessionState::Running
        );
    }

    #[test]
    fn renaming_refuses_an_empty_title() {
        let fx = Fixture::new();
        let created = fx.store.create("p1", Some("Keep")).expect("create");

        let err = fx
            .store
            .rename(&created.id, "   ", SessionState::Idle)
            .expect_err("an empty title is refused");
        assert!(matches!(err, AppError::SessionTitle), "{err:?}");

        let detail = fx
            .store
            .open(&created.id, SessionState::Idle)
            .expect("open");
        assert_eq!(detail.session.title, "Keep");
    }

    #[test]
    fn a_tool_call_status_can_be_advanced_without_losing_its_summary() {
        let fx = Fixture::new();
        let created = fx.store.create("p1", None).expect("create");

        fx.store
            .append(
                &created.id,
                Message::assistant(
                    "",
                    vec![ToolCallRecord {
                        call_id: "call_1".to_owned(),
                        tool: "fs_read".to_owned(),
                        args_json: r#"{"path":"a.txt"}"#.to_owned(),
                        status: ToolCallStatus::Pending,
                        summary: None,
                        image_path: None,
                    }],
                ),
                SessionState::Running,
            )
            .expect("append");

        assert!(fx
            .store
            .set_tool_call_status(
                &created.id,
                "call_1",
                ToolCallStatus::Ok,
                Some("read a.txt (12 B)".to_owned()),
                None,
            )
            .expect("update"));

        // A later status change that carries no summary keeps the one on file.
        assert!(fx
            .store
            .set_tool_call_status(&created.id, "call_1", ToolCallStatus::Cancelled, None, None)
            .expect("update"));

        let detail = fx
            .store
            .open(&created.id, SessionState::Idle)
            .expect("open");
        let call = &detail.messages[0].tool_calls[0];
        assert_eq!(call.status, ToolCallStatus::Cancelled);
        assert_eq!(call.summary.as_deref(), Some("read a.txt (12 B)"));
    }

    #[test]
    fn updating_an_unknown_call_reports_it_rather_than_failing() {
        let fx = Fixture::new();
        let created = fx.store.create("p1", None).expect("create");

        let found = fx
            .store
            .set_tool_call_status(&created.id, "nope", ToolCallStatus::Ok, None, None)
            .expect("no error");
        assert!(!found);
    }

    #[test]
    fn deleting_a_project_takes_its_sessions_with_it() {
        let fx = Fixture::new();
        fx.store.create("p1", None).expect("create");
        fx.store.create("p1", None).expect("create");
        let kept = fx.store.create("p2", None).expect("create");

        assert_eq!(fx.store.delete_for_project("p1").expect("delete"), 2);
        assert!(fx.store.list("p1", &idle).is_empty());
        assert_eq!(fx.store.list("p2", &idle).len(), 1);
        assert!(fx.store.open(&kept.id, SessionState::Idle).is_ok());
    }

    #[test]
    fn an_unknown_session_is_a_stale_list_not_a_crash() {
        let fx = Fixture::new();

        let err = fx
            .store
            .open("missing", SessionState::Idle)
            .expect_err("no such session");
        assert!(matches!(err, AppError::SessionNotFound { .. }), "{err:?}");

        assert!(fx.store.delete("missing").is_err());
        assert!(fx.store.messages("missing").is_err());
        assert!(fx.store.project_of("missing").is_err());
    }

    #[test]
    fn a_damaged_document_is_moved_aside_rather_than_blocking_the_app() {
        let fx = Fixture::new();
        fx.store.create("p1", Some("Gone")).expect("create");

        fs::write(fx.document(), b"{ not json").expect("damage the document");
        let reopened = fx.reopen();

        assert!(reopened.list("p1", &idle).is_empty());
        let quarantined: Vec<_> = fs::read_dir(&fx.data)
            .expect("read data dir")
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains("corrupt-"))
            .collect();
        assert_eq!(quarantined.len(), 1, "the original is kept, not deleted");
    }

    #[test]
    fn a_future_schema_version_is_quarantined_rather_than_guessed_at() {
        let fx = Fixture::new();
        fs::write(
            fx.document(),
            br#"{"version":99,"sessions":[{"id":"s","project_id":"p","title":"t","created_at":"","updated_at":"","messages":[]}]}"#,
        )
        .expect("write a future document");

        assert!(fx.reopen().list("p", &idle).is_empty());
    }

    /// `src/ipc/bindings.ts` is generated from these structs, so a renamed
    /// field reaches TypeScript on its own. What generation cannot check is
    /// that the names still match the contract in `PLAN.md` § 2.1 — a rename
    /// would regenerate happily and silently change the wire format.
    #[test]
    fn payloads_carry_the_documented_field_names() {
        let detail = SessionDetail {
            session: SessionSummary {
                id: "s".to_owned(),
                project_id: "p".to_owned(),
                title: "First".to_owned(),
                created_at: "2026-08-28T09:41:07.412Z".to_owned(),
                updated_at: "2026-08-28T09:41:07.412Z".to_owned(),
                message_count: 3,
                state: SessionState::AwaitingApproval,
            },
            messages: vec![Message::assistant(
                "done",
                vec![ToolCallRecord {
                    call_id: "call_1".to_owned(),
                    tool: "fs_read".to_owned(),
                    args_json: "{}".to_owned(),
                    status: ToolCallStatus::Ok,
                    summary: None,
                    image_path: None,
                }],
            )],
            pending_approvals: Vec::new(),
        };

        let json = serde_json::to_value(&detail).expect("SessionDetail serializes");

        let fields = |value: &serde_json::Value| -> Vec<String> {
            let mut keys: Vec<String> = value
                .as_object()
                .expect("a JSON object")
                .keys()
                .cloned()
                .collect();
            keys.sort();
            keys
        };
        let sorted = |names: &[&str]| -> Vec<String> {
            let mut out: Vec<String> = names.iter().map(|s| (*s).to_owned()).collect();
            out.sort();
            out
        };

        assert_eq!(
            fields(&json),
            sorted(&["session", "messages", "pending_approvals"])
        );
        assert_eq!(
            fields(&json["session"]),
            sorted(&[
                "id",
                "project_id",
                "title",
                "created_at",
                "updated_at",
                "message_count",
                "state",
            ])
        );
        assert_eq!(
            fields(&json["messages"][0]),
            sorted(&[
                "id",
                "role",
                "text",
                "tool_calls",
                "tool_call_id",
                "created_at"
            ])
        );
        assert_eq!(
            fields(&json["messages"][0]["tool_calls"][0]),
            sorted(&[
                "call_id",
                "tool",
                "args_json",
                "status",
                "summary",
                "image_path"
            ])
        );

        // The enums cross the wire as the snake_case strings the UI branches on.
        assert_eq!(json["session"]["state"], "awaiting_approval");
        assert_eq!(json["messages"][0]["role"], "assistant");
        assert_eq!(json["messages"][0]["tool_calls"][0]["status"], "ok");

        // `None` must reach TypeScript as `null`, not as a missing key.
        assert!(json["messages"][0]["tool_call_id"].is_null());
        assert!(json["messages"][0]["tool_calls"][0]["summary"].is_null());
    }

    #[test]
    fn a_turn_handle_carries_both_ids() {
        let json = serde_json::to_value(TurnHandle {
            session_id: "s".to_owned(),
            turn_id: "t".to_owned(),
        })
        .expect("TurnHandle serializes");

        assert_eq!(json["session_id"], "s");
        assert_eq!(json["turn_id"], "t");
    }
}
