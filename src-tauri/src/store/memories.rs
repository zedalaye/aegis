//! The memory document: `memories.json` (PLAN 7.3, Phase 14; `COS.md`
//! *Memory*).
//!
//! The third of `COS.md`'s four layers. The source of truth is the real tools;
//! the shared workspace is files in the user's own folder (Phase 11); the
//! session context is the chat. This is **role memory**: the preferences, the
//! exceptions and the "how we do it here" that belong to one identity and
//! outlive every conversation it has.
//!
//! It is a *store*, not a second filesystem. PLAN 7.1 (*Workspace*) forbids
//! inventing a hidden agent-memory tree beside the workspace, and this is not
//! one: a memory is a short record in a JSON document beside `agents.json`,
//! with no paths, no directories and nothing for a tool to traverse. Anything
//! that wants to be a file belongs in the workspace, written with `fs_write`
//! under the approval gate like a decision or a status.
//!
//! Four decisions shape it.
//!
//! **A memory is one of three things.** [`MemoryKind`] is the whole vocabulary
//! — a preference, an exception, or a convention — because those are the three
//! `COS.md` names, and because a store that accepted anything would become the
//! transcript it exists to replace. A note that is none of them is not a
//! memory: it is either a fact about the project, which is a file, or a
//! procedure, which is a skill (PLAN 7.6, *Not a skill*).
//!
//! **A memory is scoped to one identity and never leaks.** Every record
//! carries an `agent_id`, every accessor takes one, and there is no query that
//! spans identities. That is what makes "clone a role without cloning its
//! rotten memory" (`COS.md` *Loop*) something the data model allows, rather
//! than something a later phase has to retrofit.
//!
//! **A memory cites, or it is a hypothesis.** [`Memory::source`] is
//! `COS.md`'s *cite* operation made structural: an important memory points at
//! a file, a ticket or a person rather than at "I remember that". It is
//! optional, because a preference someone stated out loud has no path — but
//! the field is there, it reaches the prompt, and its absence is as visible as
//! its presence.
//!
//! **The model may write and read; only the human forgets.** The tools
//! ([`tools::memory`](crate::tools::memory)) are `memory_write` and
//! `memory_search`, and there is deliberately no `memory_forget` among them:
//! `COS.md` gives the human "memory correction" as one of the three things
//! that role exists for, deleting is irreversible, and irreversible actions
//! stay behind a human gate (`AGENTS.md` *Permissions*). [`MemoryStore::save`]
//! and [`MemoryStore::forget`] are reached from the Memory panel, which is
//! where a person corrects a stale hypothesis.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use super::{now, quarantine, strip_bom, write_atomic};
use crate::error::{AppError, AppResult};

/// Name of the document under the application-data directory.
const MEMORIES_FILE: &str = "memories.json";

/// Schema version of [`MemoriesFile`].
///
/// Its own version, independent of the other documents: they change at very
/// different rates, and a migration to one has no business quarantining the
/// others.
const SCHEMA_VERSION: u32 = 1;

/// Longest a memory may be.
///
/// A sentence, not a paragraph. The cap is the point: role memory holds
/// exceptions and preferences, and a memory long enough to hold a procedure is
/// a runbook someone declined to write down (PLAN 7.6).
pub const TEXT_MAX_CHARS: usize = 280;

/// Longest a citation may be.
pub const SOURCE_MAX_CHARS: usize = 200;

/// Most memories one identity may hold.
///
/// A ceiling on the store, not on the prompt — [`PROMPT_MAX`] is that. Past it
/// the answer is to forget something, which is what the refusal actually says,
/// rather than to evict the oldest silently: what would be evicted is as
/// likely to be a human's correction as the model's own guess.
pub const MEMORIES_MAX: usize = 200;

/// Most memories carried in a system message.
pub const PROMPT_MAX: usize = 20;

/// Most bytes of memory carried in a system message.
///
/// The second half of the same rule. Twenty memories at the character cap
/// would be six kilobytes on every single request, which is the workspace
/// digest's mistake made twice.
pub const PROMPT_MAX_BYTES: usize = 2 * 1024;

/// Most memories one `memory_search` returns.
pub const SEARCH_MAX_RESULTS: usize = 12;

// ---------------------------------------------------------------------------
// IPC payloads (PLAN 7.3, Phase 14)
// ---------------------------------------------------------------------------

/// What a memory *is*, which is also the whole vocabulary.
///
/// Three variants and no `other`. The discipline is the one the seven skill
/// headings impose: naming which of the three a memory is forces whoever
/// writes it to notice when it is none of them, and a memory that is none of
/// them belongs in a file or in a runbook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum MemoryKind {
    /// How someone likes things done: "this client wants French".
    Preference,
    /// Where the usual rule does not apply: "never touch the vendored crate".
    Exception,
    /// How it is done here: "releases are tagged before the changelog".
    Convention,
}

impl MemoryKind {
    /// The three, for a message that has to list them.
    pub const ALL: [Self; 3] = [Self::Preference, Self::Exception, Self::Convention];

    /// The wire string, which is also what the model writes.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Preference => "preference",
            Self::Exception => "exception",
            Self::Convention => "convention",
        }
    }

    /// Parses what the model sent.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "preference" => Some(Self::Preference),
            "exception" => Some(Self::Exception),
            "convention" => Some(Self::Convention),
            _ => None,
        }
    }
}

/// One memory, as the UI and the runtime see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct Memory {
    /// UUID v4.
    pub id: String,
    /// The identity this belongs to. No accessor spans two.
    pub agent_id: String,
    /// Which of the three it is.
    pub kind: MemoryKind,
    /// The memory itself, in one sentence.
    pub text: String,
    /// What it rests on: a path, a ticket, a person. `None` for a preference
    /// somebody simply stated.
    pub source: Option<String>,
    /// RFC3339, UTC.
    pub created_at: String,
    /// RFC3339, UTC. Bumped by a correction, and by a write that repeats
    /// something already held.
    pub updated_at: String,
}

impl Memory {
    /// The one line this memory contributes to a prompt or a search result.
    ///
    /// The kind leads because it says how much weight the rest carries: an
    /// exception overrides, a preference colours, a convention is the default.
    /// The citation trails in brackets, and a memory with none should read
    /// like one.
    pub fn line(&self) -> String {
        match &self.source {
            Some(source) => format!("- ({}) {} [{}]", self.kind.as_str(), self.text, source),
            None => format!("- ({}) {}", self.kind.as_str(), self.text),
        }
    }
}

/// What a create or a correction carries.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct MemoryDraft {
    /// Which of the three this is.
    pub kind: MemoryKind,
    /// The memory itself. Trimmed, and never empty.
    pub text: String,
    /// What it rests on, when it rests on something nameable.
    pub source: Option<String>,
}

// ---------------------------------------------------------------------------
// On-disk shapes
// ---------------------------------------------------------------------------

/// The document itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct MemoriesFile {
    version: u32,
    memories: Vec<StoredMemory>,
}

/// A memory as persisted.
///
/// The same fields as [`Memory`], which is unusual in this tree — the other
/// stores keep a derived field out of the record. A memory has no derived
/// field: everything about it is stored, and a second type asserting that
/// would be ceremony.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredMemory {
    id: String,
    agent_id: String,
    kind: MemoryKind,
    text: String,
    #[serde(default)]
    source: Option<String>,
    created_at: String,
    updated_at: String,
}

impl StoredMemory {
    /// The memory as everything outside this module sees it.
    fn to_memory(&self) -> Memory {
        Memory {
            id: self.id.clone(),
            agent_id: self.agent_id.clone(),
            kind: self.kind,
            text: self.text.clone(),
            source: self.source.clone(),
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// The memory store: every identity's memories, in one document.
///
/// Same shape as the other stores — one mutex over the whole list, written out
/// on every mutation — and for the same reason: at this size "what is on disk"
/// equals "what is in memory" once a call returns, with no flush to forget.
///
/// One document for every identity rather than one per identity, because the
/// scoping that matters is enforced on *access* — every method takes an
/// `agent_id`, and none of them spans two — and a file per identity would make
/// deleting one a second thing that can half-happen.
#[derive(Debug)]
pub struct MemoryStore {
    path: PathBuf,
    memories: Mutex<Vec<StoredMemory>>,
}

impl MemoryStore {
    /// Loads the store from `data_dir`.
    ///
    /// Never fails, for the reason the other stores do not: a tray app that
    /// will not boot cannot explain why it did not. A damaged document costs
    /// the memories in it and nothing else — every identity still opens and
    /// still talks, with nothing recalled.
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(MEMORIES_FILE);

        let memories = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<MemoriesFile>(strip_bom(&bytes)) {
                Ok(file) if file.version == SCHEMA_VERSION => {
                    tracing::info!(count = file.memories.len(), "memory store loaded");
                    file.memories
                }
                Ok(file) => {
                    tracing::error!(
                        found = file.version,
                        expected = SCHEMA_VERSION,
                        "unknown memory store version"
                    );
                    quarantine(&path);
                    Vec::new()
                }
                Err(err) => {
                    tracing::error!(%err, "memory store is not readable JSON");
                    quarantine(&path);
                    Vec::new()
                }
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                tracing::info!("no memory store yet; nothing is remembered");
                Vec::new()
            }
            Err(err) => {
                tracing::error!(%err, "could not read the memory store");
                Vec::new()
            }
        };

        Self {
            path,
            memories: Mutex::new(memories),
        }
    }

    /// Locks the list, recovering from a poisoned mutex.
    ///
    /// Same reasoning as the other stores: the guarded value is a `Vec` only
    /// ever replaced wholesale, so it cannot be torn, and propagating a panic
    /// through every later command is strictly worse.
    fn memories(&self) -> MutexGuard<'_, Vec<StoredMemory>> {
        self.memories
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// One identity's memories, most recently touched first.
    ///
    /// Recency rather than relevance, and deliberately: relevance needs a
    /// query, [`MemoryStore::search`] is where a query goes, and a ranking
    /// invented here would be one no caller could see or correct. What recency
    /// buys is that the memories in the prompt are the ones most recently
    /// confirmed — a write that repeats something already held touches it, so
    /// a fact that keeps coming up keeps its place.
    pub fn list_for(&self, agent_id: &str) -> Vec<Memory> {
        let memories = self.memories();

        let mut out: Vec<Memory> = memories
            .iter()
            .filter(|memory| memory.agent_id == agent_id)
            .map(StoredMemory::to_memory)
            .collect();
        // Fixed-width UTC RFC3339, so comparing the strings *is* comparing the
        // instants.
        out.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| b.created_at.cmp(&a.created_at))
        });
        out
    }

    /// How many memories one identity holds.
    pub fn count_for(&self, agent_id: &str) -> usize {
        let memories = self.memories();
        memories
            .iter()
            .filter(|memory| memory.agent_id == agent_id)
            .count()
    }

    /// One identity's memories carrying every term of `query`.
    ///
    /// Substring matching over the text and the citation, case-insensitively,
    /// with every whitespace-separated term required. No index, no stemming,
    /// no ranking: at a two-hundred-record ceiling an index would be slower
    /// than the scan, and a ranking the caller cannot see is one nobody can
    /// debug. An empty query is every memory, capped — the honest answer to
    /// "what do you know", and the same answer the panel gives.
    pub fn search(&self, agent_id: &str, query: &str) -> Vec<Memory> {
        let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();

        self.list_for(agent_id)
            .into_iter()
            .filter(|memory| matches(memory, &terms))
            .take(SEARCH_MAX_RESULTS)
            .collect()
    }

    /// Records a memory, or corrects one.
    ///
    /// `id` of `None` creates; `Some` corrects the memory with that id, which
    /// has to belong to `agent_id` — an id from another identity is *not
    /// found* rather than refused, because "there is no such memory here" is
    /// the true answer, and the alternative confirms that one exists
    /// elsewhere.
    ///
    /// A create whose text an existing memory already carries touches that one
    /// rather than storing a second copy. This is `COS.md`'s *consolidate* at
    /// the door instead of on a clock — the clock is a routine, and routines
    /// are Phase 16. What it buys now is that an identity reminded of the same
    /// preference in ten sessions holds one memory rather than ten, with a
    /// timestamp saying it is still live.
    pub fn save(&self, agent_id: &str, id: Option<&str>, draft: &MemoryDraft) -> AppResult<Memory> {
        let valid = Valid::check(draft)?;
        let mut memories = self.memories();

        if let Some(id) = id {
            let stored = Self::find_mut(&mut memories, agent_id, id)?;
            stored.kind = valid.kind;
            stored.text = valid.text;
            stored.source = valid.source;
            stored.updated_at = now();
            let corrected = stored.to_memory();

            self.write(&memories)?;
            tracing::info!(id, agent_id, "memory corrected");
            return Ok(corrected);
        }

        // Consolidation before the cap, so repeating something already held
        // never fails on a full store: it is not adding anything.
        if let Some(held) = memories
            .iter_mut()
            .find(|memory| memory.agent_id == agent_id && same_text(&memory.text, &valid.text))
        {
            held.kind = valid.kind;
            // A citation is only ever added by a repeat, never removed by one:
            // the second telling of a preference rarely repeats where it came
            // from, and dropping the source would make a memory weaker every
            // time it was confirmed.
            held.source = valid.source.or_else(|| held.source.clone());
            held.updated_at = now();
            let touched = held.to_memory();

            self.write(&memories)?;
            tracing::debug!(id = %touched.id, agent_id, "memory already held; touched");
            return Ok(touched);
        }

        let count = memories
            .iter()
            .filter(|memory| memory.agent_id == agent_id)
            .count();
        if count >= MEMORIES_MAX {
            return Err(AppError::Memory {
                field: "text",
                reason: format!(
                    "this identity already holds {MEMORIES_MAX} memories, which is the cap. \
                     Forget one that no longer holds before adding another"
                ),
            });
        }

        let stamp = now();
        let stored = StoredMemory {
            id: Uuid::new_v4().to_string(),
            agent_id: agent_id.to_owned(),
            kind: valid.kind,
            text: valid.text,
            source: valid.source,
            created_at: stamp.clone(),
            updated_at: stamp,
        };
        let created = stored.to_memory();

        memories.push(stored);
        self.write(&memories)?;

        tracing::info!(
            id = %created.id,
            agent_id,
            kind = created.kind.as_str(),
            "memory recorded"
        );
        Ok(created)
    }

    /// Forgets one memory, and reports what went.
    ///
    /// Scoped like every other method: an id belonging to another identity is
    /// not found. It returns the record so a caller can say *what* was
    /// forgotten rather than only that something was.
    pub fn forget(&self, agent_id: &str, id: &str) -> AppResult<Memory> {
        let mut memories = self.memories();

        let at = memories
            .iter()
            .position(|memory| memory.agent_id == agent_id && memory.id == id)
            .ok_or_else(|| AppError::MemoryNotFound { id: id.to_owned() })?;
        let forgotten = memories.remove(at).to_memory();

        self.write(&memories)?;
        tracing::info!(id, agent_id, "memory forgotten");
        Ok(forgotten)
    }

    /// Forgets everything one identity held, and reports how much went.
    ///
    /// Called when an identity is deleted. Memories of an identity that no
    /// longer exists are unreachable — every method takes an `agent_id` — and
    /// leaving them behind would grow the document forever with records
    /// nothing can name.
    pub fn forget_for_agent(&self, agent_id: &str) -> AppResult<usize> {
        let mut memories = self.memories();

        let before = memories.len();
        memories.retain(|memory| memory.agent_id != agent_id);
        let removed = before - memories.len();

        if removed > 0 {
            self.write(&memories)?;
            tracing::info!(agent_id, removed, "memories forgotten with their identity");
        }
        Ok(removed)
    }

    /// Looks a memory up within one identity, mutably.
    fn find_mut<'a>(
        memories: &'a mut [StoredMemory],
        agent_id: &str,
        id: &str,
    ) -> AppResult<&'a mut StoredMemory> {
        memories
            .iter_mut()
            .find(|memory| memory.agent_id == agent_id && memory.id == id)
            .ok_or_else(|| AppError::MemoryNotFound { id: id.to_owned() })
    }

    /// Serializes the list and replaces the document atomically.
    ///
    /// Takes the guard, so the only way to reach it is to already hold the
    /// lock: a caller cannot mutate the list and forget to persist it.
    fn write(&self, memories: &[StoredMemory]) -> AppResult<()> {
        let file = MemoriesFile {
            version: SCHEMA_VERSION,
            memories: memories.to_vec(),
        };
        let bytes = serde_json::to_vec_pretty(&file).map_err(|err| AppError::Store {
            action: "serialize",
            source: io::Error::other(err),
        })?;

        write_atomic(&self.path, &bytes).map_err(|err| {
            tracing::error!(%err, path = %self.path.display(), "could not write the memory store");
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

// ---------------------------------------------------------------------------
// Recall
// ---------------------------------------------------------------------------

/// Whether a memory carries every term.
fn matches(memory: &Memory, terms: &[String]) -> bool {
    if terms.is_empty() {
        return true;
    }
    let haystack = match &memory.source {
        Some(source) => format!("{} {source}", memory.text).to_lowercase(),
        None => memory.text.to_lowercase(),
    };
    terms.iter().all(|term| haystack.contains(term))
}

/// Whether two memories say the same thing, for consolidation.
///
/// Case and surrounding whitespace only. Nothing cleverer: a store that decided
/// two differently worded memories were "the same" would be silently discarding
/// one of them, and the one it discarded would sometimes be the human's.
fn same_text(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// The memory block for a system message, or `None` when nothing is held.
///
/// `memories` is one identity's, most recent first
/// ([`MemoryStore::list_for`]); `total` is how many it holds, so a block that
/// had to stop can say what it left behind and how to reach it.
///
/// This is the *re-inject* half of PLAN 7.3's retrieve-after-compact. Because
/// the block is rebuilt into every system message, a compaction cannot take it
/// away: what folds is the conversation, and what stands is what the identity
/// knows. There is no separate "after compaction, restore memory" step for the
/// same reason there is no "after compaction, restore the workspace digest" —
/// neither of them was ever in the part that folds.
///
/// Capped twice, by count and by bytes, for the reason the workspace digest is
/// (PLAN 7.1, *System prompt*): this is paid for on every single request, and a
/// store that has been running for a year must not silently start costing a
/// context window per turn.
pub fn prompt_block(memories: &[Memory], total: usize) -> Option<String> {
    if memories.is_empty() {
        return None;
    }

    let mut block = String::from(
        "What you have learned working as this identity. These are yours, not the user's \
         standing instructions: an exception overrides the usual rule, a preference colours how \
         you do something, a convention is the default here. A bracketed source is what the \
         memory rests on — read it rather than repeating the memory as though it were proof — \
         and one with no source is a hypothesis. You cannot delete these: if any of them is \
         wrong, say so plainly and the user will correct it.\n",
    );

    let mut shown = 0;
    for memory in memories.iter().take(PROMPT_MAX) {
        let line = memory.line();
        if block.len() + line.len() + 1 > PROMPT_MAX_BYTES {
            break;
        }
        block.push('\n');
        block.push_str(&line);
        shown += 1;
    }

    // Reachable only when the first line is already over the cap, which the
    // character cap on the text makes unlikely and does not make impossible.
    if shown == 0 {
        return None;
    }

    if shown < total {
        block.push_str(&format!(
            "\n\n{} more are held. `memory_search` finds them by word.",
            total - shown
        ));
    }

    Some(block)
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// A draft that has been checked, with every field in the form it is stored in.
///
/// A separate type rather than validating in place, for the reason the agent
/// store's is: at the point of writing there is then no way to confuse a field
/// that was checked with one that was merely trimmed.
struct Valid {
    kind: MemoryKind,
    text: String,
    source: Option<String>,
}

impl Valid {
    /// Checks a draft.
    ///
    /// Every message is written for whoever has to fix it — the person
    /// correcting a form, or the model reading a refused call — so it says what
    /// is wrong *and* what a working value looks like.
    fn check(draft: &MemoryDraft) -> AppResult<Self> {
        let text = draft.text.trim();
        if text.is_empty() {
            return Err(AppError::Memory {
                field: "text",
                reason: "say what is worth remembering, in one sentence — \"this client wants \
                         French\""
                    .to_owned(),
            });
        }
        if text.chars().count() > TEXT_MAX_CHARS {
            return Err(AppError::Memory {
                field: "text",
                reason: format!(
                    "keep it under {TEXT_MAX_CHARS} characters. A memory is a preference or an \
                     exception, not a procedure — a procedure is a skill, and a fact about the \
                     project is a file in the workspace"
                ),
            });
        }

        let source = match draft.source.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(source) if source.chars().count() > SOURCE_MAX_CHARS => {
                return Err(AppError::Memory {
                    field: "source",
                    reason: format!(
                        "a source is a pointer, not a quotation — keep it under \
                         {SOURCE_MAX_CHARS} characters, like `.aegis/decisions/DECISIONS.md` or a \
                         ticket id"
                    ),
                })
            }
            Some(source) => Some(source.to_owned()),
        };

        Ok(Self {
            kind: draft.kind,
            text: text.to_owned(),
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    fn draft(kind: MemoryKind, text: &str) -> MemoryDraft {
        MemoryDraft {
            kind,
            text: text.to_owned(),
            source: None,
        }
    }

    fn store() -> (TempDir, MemoryStore) {
        let dir = TempDir::new().expect("temp dir");
        let store = MemoryStore::load(dir.path());
        (dir, store)
    }

    #[test]
    fn a_memory_is_scoped_to_the_identity_that_wrote_it() {
        let (_dir, store) = store();

        store
            .save(
                "a",
                None,
                &draft(MemoryKind::Preference, "answers in French"),
            )
            .expect("recorded");
        store
            .save(
                "b",
                None,
                &draft(MemoryKind::Convention, "tags before the changelog"),
            )
            .expect("recorded");

        let mine = store.list_for("a");
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].text, "answers in French");
        assert_eq!(store.count_for("b"), 1);
        assert!(store.list_for("c").is_empty(), "and nothing leaks sideways");
    }

    /// The one thing an id from another identity must not do is confirm that
    /// it exists.
    #[test]
    fn another_identitys_memory_is_not_found_rather_than_refused() {
        let (_dir, store) = store();
        let theirs = store
            .save(
                "b",
                None,
                &draft(MemoryKind::Exception, "never touch vendor/"),
            )
            .expect("recorded");

        let err = store
            .forget("a", &theirs.id)
            .expect_err("not theirs to forget");
        assert!(matches!(err, AppError::MemoryNotFound { .. }), "{err:?}");
        assert_eq!(store.count_for("b"), 1, "and it is still there");
    }

    /// `COS.md` *consolidate*, at the door rather than on a clock: an identity
    /// told the same thing in ten sessions holds one memory, not ten.
    #[test]
    fn writing_something_already_held_touches_it_instead_of_duplicating() {
        let (_dir, store) = store();

        let first = store
            .save(
                "a",
                None,
                &draft(MemoryKind::Preference, "answers in French"),
            )
            .expect("recorded");
        let again = store
            .save(
                "a",
                None,
                &MemoryDraft {
                    source: Some(".aegis/briefs/client.md".to_owned()),
                    ..draft(MemoryKind::Preference, "  Answers In French  ")
                },
            )
            .expect("recorded again");

        assert_eq!(again.id, first.id, "the same record");
        assert_eq!(store.count_for("a"), 1);
        assert_eq!(
            again.source.as_deref(),
            Some(".aegis/briefs/client.md"),
            "a repeat may add a citation"
        );

        // And a repeat that names no source does not take one away.
        let third = store
            .save(
                "a",
                None,
                &draft(MemoryKind::Preference, "answers in french"),
            )
            .expect("recorded a third time");
        assert_eq!(third.source.as_deref(), Some(".aegis/briefs/client.md"));
    }

    #[test]
    fn a_correction_keeps_the_id_so_nothing_is_orphaned() {
        let (_dir, store) = store();
        let held = store
            .save(
                "a",
                None,
                &draft(MemoryKind::Preference, "answers in French"),
            )
            .expect("recorded");

        let fixed = store
            .save(
                "a",
                Some(&held.id),
                &draft(MemoryKind::Exception, "answers in German"),
            )
            .expect("corrected");

        assert_eq!(fixed.id, held.id);
        assert_eq!(fixed.kind, MemoryKind::Exception);
        assert_eq!(store.count_for("a"), 1);
    }

    #[test]
    fn the_caps_are_refused_in_words_that_say_what_to_do() {
        let (_dir, store) = store();

        let empty = store
            .save("a", None, &draft(MemoryKind::Preference, "   "))
            .expect_err("an empty memory is nothing");
        assert!(empty.to_string().contains("one sentence"), "{empty}");

        let long = "x".repeat(TEXT_MAX_CHARS + 1);
        let err = store
            .save("a", None, &draft(MemoryKind::Preference, &long))
            .expect_err("a memory is not a procedure");
        assert!(err.to_string().contains("a skill"), "{err}");
    }

    #[test]
    fn a_full_store_says_to_forget_something_rather_than_evicting() {
        let (_dir, store) = store();
        for n in 0..MEMORIES_MAX {
            store
                .save(
                    "a",
                    None,
                    &draft(MemoryKind::Convention, &format!("rule {n}")),
                )
                .expect("recorded");
        }

        let err = store
            .save(
                "a",
                None,
                &draft(MemoryKind::Convention, "one rule too many"),
            )
            .expect_err("the cap holds");
        assert!(err.to_string().contains("Forget one"), "{err}");
        assert_eq!(store.count_for("a"), MEMORIES_MAX, "nothing was evicted");

        // A repeat still lands, because it adds nothing.
        store
            .save("a", None, &draft(MemoryKind::Convention, "rule 0"))
            .expect("a repeat is not an addition");
    }

    #[test]
    fn search_requires_every_term_and_looks_at_the_citation_too() {
        let (_dir, store) = store();
        store
            .save(
                "a",
                None,
                &MemoryDraft {
                    source: Some(".aegis/decisions/DECISIONS.md".to_owned()),
                    ..draft(MemoryKind::Convention, "releases are tagged first")
                },
            )
            .expect("recorded");
        store
            .save(
                "a",
                None,
                &draft(MemoryKind::Preference, "answers in French"),
            )
            .expect("recorded");

        assert_eq!(store.search("a", "tagged releases").len(), 1);
        assert_eq!(store.search("a", "tagged French").len(), 0, "every term");
        assert_eq!(
            store.search("a", "DECISIONS").len(),
            1,
            "the citation counts"
        );
        assert_eq!(store.search("a", "  ").len(), 2, "no query is everything");
    }

    #[test]
    fn deleting_an_identity_takes_its_memories_with_it() {
        let (_dir, store) = store();
        store
            .save(
                "a",
                None,
                &draft(MemoryKind::Preference, "answers in French"),
            )
            .expect("recorded");
        store
            .save(
                "b",
                None,
                &draft(MemoryKind::Preference, "answers in Dutch"),
            )
            .expect("recorded");

        assert_eq!(store.forget_for_agent("a").expect("forgotten"), 1);
        assert_eq!(store.count_for("a"), 0);
        assert_eq!(store.count_for("b"), 1, "and nobody else's");
    }

    #[test]
    fn memories_survive_a_restart() {
        let dir = TempDir::new().expect("temp dir");
        {
            let store = MemoryStore::load(dir.path());
            store
                .save(
                    "a",
                    None,
                    &draft(MemoryKind::Exception, "never touch vendor/"),
                )
                .expect("recorded");
        }

        let reopened = MemoryStore::load(dir.path());
        let held = reopened.list_for("a");
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].kind, MemoryKind::Exception);
    }

    #[test]
    fn the_prompt_block_says_what_it_left_behind() {
        let (_dir, store) = store();
        for n in 0..(PROMPT_MAX + 3) {
            store
                .save(
                    "a",
                    None,
                    &draft(MemoryKind::Convention, &format!("rule {n}")),
                )
                .expect("recorded");
        }

        let held = store.list_for("a");
        let block = prompt_block(&held, held.len()).expect("a block");

        assert_eq!(block.matches("\n- (").count(), PROMPT_MAX);
        assert!(block.contains("3 more are held"), "{block}");
        assert!(block.contains("memory_search"), "and how to reach them");
        assert!(
            block.len() <= PROMPT_MAX_BYTES + 128,
            "the block stays small"
        );
    }

    #[test]
    fn nothing_held_is_no_block_at_all() {
        assert!(prompt_block(&[], 0).is_none());
    }
}
