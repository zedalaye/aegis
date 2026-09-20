//! The shared-workspace convention, the **cabinet** (Phases 11 and 13;
//! PLAN 7.2; `COS.md` *Memory*; `docs/guide/workspace.md`).
//!
//! `briefs/`, `status/`, `artefacts/`, `decisions/` and `skills/` under one
//! [`CABINET_DIR`]. They are ordinary files reached through the same path
//! resolution, gate and audit log as any other, so no hidden store (PLAN 7.1).
//! The dot hides them from Finder and `rg` by default. The constitution lives
//! beside it at the root in [`world`](crate::world).
//!
//! * **scaffold** — [`scaffold`] creates what is missing, never overwrites, and
//!   makes the folder a git work tree without committing (PLAN 7.11).
//! * **read** — [`digest`] is rebuilt for every model request: capped excerpts of
//!   the two state files and file *names* of briefs and artefacts, never
//!   procedure or content. `skills/` reaches the model as the skill catalog.
//! * **write** — no tool here: files are written with `fs_write` under the gate;
//!   [`PREAMBLE`] says where things go.

use std::fs;
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use serde::Serialize;
use ts_rs::TS;

use crate::error::{AppError, AppResult};
use crate::git::{self, Versioning};
use crate::policy::path;
use crate::skills;

/// The directory the cabinet lives under, at the workspace root.
///
/// The paths below spell it out (`concat!` cannot use a `const`);
/// [`every_convention_path_is_under_the_cabinet`] keeps them in step.
///
/// [`every_convention_path_is_under_the_cabinet`]: self#tests
pub const CABINET_DIR: &str = ".aegis";

/// Where a delegated brief is filed (Phase 15). The handoff bus never creates
/// it.
pub const BRIEFS_DIR: &str = ".aegis/briefs";

/// Where produced work goes. The explorer refuses drops aimed here (PLAN 7.15).
pub const ARTEFACTS_DIR: &str = ".aegis/artefacts";

/// The state file a session reads first: what is true now.
pub const STATUS_FILE: &str = ".aegis/status/STATUS.md";

/// The ledger a decision is filed in, instead of in the transcript.
pub const DECISIONS_FILE: &str = ".aegis/decisions/DECISIONS.md";

/// Most bytes of one state file that reach the system message; the digest says
/// what was elided.
pub const EXCERPT_MAX_BYTES: u64 = 2 * 1024;

/// Most file names listed for `.aegis/briefs/` and `.aegis/artefacts/`.
const LISTING_MAX_ENTRIES: usize = 12;

/// How a slot reaches the model, when it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Digest {
    /// The names of the directory's entries, never their content.
    Listing,
    /// The first [`EXCERPT_MAX_BYTES`] of the file. For a board that is
    /// rewritten in place, the top is the current state.
    Head,
    /// The last [`EXCERPT_MAX_BYTES`] of the file. For a ledger that is
    /// appended to, the end is the recent decisions.
    Tail,
}

/// One directory of the convention, its seed file, and how it is read back.
/// [`layout`], [`scaffold`] and [`digest`] all walk the table.
struct Slot {
    /// The bare name inside [`CABINET_DIR`]; [`strays`] also looks for it at
    /// the root, where an older layout put it.
    name: &'static str,
    /// The file [`scaffold`] seeds inside it.
    file: &'static str,
    /// What that file starts as. Written once, never rewritten.
    seed: &'static str,
    /// What the slot contributes to [`digest`]. `None` for `skills/`, which
    /// reaches the model as the catalog.
    digest: Option<Digest>,
}

impl Slot {
    /// The directory's path relative to the workspace root: `.aegis/briefs`.
    fn rel_dir(&self) -> String {
        format!("{CABINET_DIR}/{}", self.name)
    }

    /// The seed file's path relative to the workspace root, with a `/`
    /// separator — the form the convention is written in, and one
    /// [`path::resolve`] accepts on every platform.
    fn rel_file(&self) -> String {
        format!("{CABINET_DIR}/{}/{}", self.name, self.file)
    }
}

/// The convention, in the order work moves through it.
const CONVENTION: [Slot; 5] = [
    Slot {
        name: "briefs",
        file: "README.md",
        seed: BRIEFS_SEED,
        digest: Some(Digest::Listing),
    },
    Slot {
        name: "status",
        file: "STATUS.md",
        seed: STATUS_SEED,
        digest: Some(Digest::Head),
    },
    Slot {
        name: "artefacts",
        file: "README.md",
        seed: ARTEFACTS_SEED,
        digest: Some(Digest::Listing),
    },
    Slot {
        name: "decisions",
        file: "DECISIONS.md",
        seed: DECISIONS_SEED,
        digest: Some(Digest::Tail),
    },
    // Phase 13: the project's own runbooks, seeded with the `inbox.triage`
    // example.
    Slot {
        name: skills::LIBRARY_DIR,
        file: TRIAGE_FILE,
        seed: skills::TRIAGE_SEED,
        digest: None,
    },
];

/// The stub runbook seeded into a workspace, relative to `skills/` — two levels
/// deep, so [`scaffold`] creates its parent.
const TRIAGE_FILE: &str = "inbox.triage/SKILL.md";

/// What the model is told once the convention is present: where things live,
/// and that decisions go in files. No procedure.
const PREAMBLE: &str = "\
This workspace keeps its shared memory in files, under `.aegis/`. Delegated \
work is briefed in `.aegis/briefs/`, what is true right now is in \
`.aegis/status/STATUS.md`, anything produced goes in `.aegis/artefacts/`, and \
decisions are recorded in `.aegis/decisions/DECISIONS.md`. A decision or a \
status belongs in its file, not in this conversation: the conversation is \
forgotten, the file is not. `fs_read` a file before you change it and write it \
back whole, because `fs_write` replaces. What follows is the state as of the \
start of this reply.";

const BRIEFS_SEED: &str = r#"# .aegis/briefs/

One file per delegated piece of work. A brief is what you hand to whoever does
the job — a person or an agent — instead of a conversation for them to read.

    goal:
    owner:
    priority:
    inputs:              # paths and links, not pasted text
    constraints:
    definition_of_done:
    approval_needed:
    return_format:       # status | artefact | question

Inputs are paths and links. A brief that says "read my thread" is not a brief.
"#;

const STATUS_SEED: &str = "\
# Status

What is true right now. Rewritten in place, not appended to — this is a board,
not a log. Keep it short enough to read on one screen; anything longer belongs
in an artefact this file points at.

## Attention

_Nothing waiting on a human._

## In flight

_Nothing running._

## Blocked

_Nothing blocked._
";

const ARTEFACTS_SEED: &str = "\
# .aegis/artefacts/

What was produced: a draft, a report, an export, a patch. Artefacts are named
by path from a brief, a status line or a decision — never pasted into a
conversation, which is the one place they cannot survive.
";

const DECISIONS_SEED: &str = "\
# Decisions

One entry per decision, newest last. A decision that lives only in a chat is
gone at the next compaction, and nobody can tell afterwards whether it was
decided or merely discussed.

    ## YYYY-MM-DD — what was decided
    Context:   what forced the choice
    Decision:  what we are doing
    Because:   the reason, in one line
    Revisit:   what would change our mind

<!-- new entries below -->
";

// ---------------------------------------------------------------------------
// IPC payloads
// ---------------------------------------------------------------------------

/// One directory of the convention, as the UI sees it, measured on every read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct WorkspaceEntry {
    /// The directory relative to the workspace root: `.aegis/briefs`, …
    pub dir: String,
    /// The seed file inside it, relative to the root:
    /// `.aegis/status/STATUS.md`, …
    pub file: String,
    /// Whether the directory is there right now.
    pub dir_exists: bool,
    /// Whether the seed file is there right now.
    pub file_exists: bool,
}

/// The convention's state in one workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct WorkspaceLayout {
    /// The workspace root the entries are relative to.
    pub root: String,
    /// Every directory of the convention, present or not.
    pub entries: Vec<WorkspaceEntry>,
    /// Whether every directory and seed file is there.
    pub complete: bool,
    /// Convention directories still at the workspace *root* from the pre-`.aegis`
    /// layout (bare names). Reported, never moved.
    pub strays: Vec<String>,
    /// Whether the folder is in a git work tree, and whose (PLAN 7.11).
    pub versioning: Versioning,
}

/// What one scaffolding run did: files created and files kept untouched,
/// relative to the root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct ScaffoldReport {
    /// The workspace root the paths are relative to.
    pub root: String,
    /// Files created by this run, in convention order.
    pub created: Vec<String>,
    /// Files that were already there and were left exactly as they were.
    pub kept: Vec<String>,
    /// Where the history of these files is kept, measured after the run.
    pub versioning: Versioning,
    /// Whether this run created the work tree (vs. one already there).
    pub initialized: bool,
    /// Why the folder is still not versioned — usually `git` missing, which is
    /// not a failure of the scaffold.
    pub problem: Option<String>,
}

impl ScaffoldReport {
    /// Folds the git half into the report; it runs on the project's execution
    /// host (PLAN 7.12).
    #[must_use]
    pub fn versioned(mut self, ensured: git::Ensured) -> Self {
        self.versioning = ensured.versioning;
        self.initialized = ensured.initialized;
        self.problem = ensured.problem;
        self
    }
}

// ---------------------------------------------------------------------------
// Read
// ---------------------------------------------------------------------------

/// Which parts of the convention exist in `root` right now. Never fails: an
/// unreadable workspace reports everything absent.
pub fn layout(root: &Path) -> WorkspaceLayout {
    let entries: Vec<WorkspaceEntry> = CONVENTION
        .iter()
        .map(|slot| {
            let rel_dir = slot.rel_dir();
            let rel_file = slot.rel_file();
            WorkspaceEntry {
                dir_exists: inside(root, &rel_dir).is_some_and(|dir| dir.is_dir()),
                file_exists: inside(root, &rel_file).is_some_and(|file| file.is_file()),
                dir: rel_dir,
                file: rel_file,
            }
        })
        .collect();

    WorkspaceLayout {
        root: root.display().to_string(),
        complete: entries
            .iter()
            .all(|entry| entry.dir_exists && entry.file_exists),
        strays: strays(root),
        // A `.git` walk, not `git rev-parse`: cheap, and works without git.
        versioning: git::measure(root),
        entries,
    }
}

/// Convention directories still at the workspace root, detected by their seed
/// file (`status/STATUS.md`), not the directory name, which is often the
/// project's own.
fn strays(root: &Path) -> Vec<String> {
    CONVENTION
        .iter()
        .filter(|slot| {
            inside(root, &format!("{}/{}", slot.name, slot.file)).is_some_and(|file| file.is_file())
        })
        .map(|slot| slot.name.to_owned())
        .collect()
}

/// The shared state as a system-message block, or `None` without the
/// convention. Rebuilt per request, so a write is visible on the next round.
pub fn digest(root: &Path) -> Option<String> {
    let sections: Vec<String> = CONVENTION
        .iter()
        .filter_map(|slot| match slot.digest? {
            Digest::Listing => listing(root, slot),
            Digest::Head | Digest::Tail => excerpt(root, slot),
        })
        .collect();

    if sections.is_empty() {
        return None;
    }
    Some(format!("{PREAMBLE}\n\n{}", sections.join("\n\n")))
}

/// `.aegis/briefs/: intake.md, q3-plan.md` — names only, never content.
fn listing(root: &Path, slot: &Slot) -> Option<String> {
    let rel_dir = slot.rel_dir();
    let dir = inside(root, &rel_dir)?;
    let read = match fs::read_dir(&dir) {
        Ok(read) => read,
        Err(err) => {
            tracing::debug!(%err, dir = %dir.display(), "no shared directory to list");
            return None;
        }
    };

    let mut names: Vec<String> = read
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();

    let total = names.len();
    let shown = total.min(LISTING_MAX_ENTRIES);
    names.truncate(shown);

    let mut line = format!("{rel_dir}/: ");
    if total == 0 {
        line.push_str("(empty)");
    } else {
        line.push_str(&names.join(", "));
        if total > shown {
            line.push_str(&format!(", and {} more", total - shown));
        }
    }
    Some(line)
}

/// `.aegis/status/STATUS.md:` and the capped content beneath it.
fn excerpt(root: &Path, slot: &Slot) -> Option<String> {
    let rel = slot.rel_file();
    let path = inside(root, &rel)?;
    let digest = slot.digest?;

    let (text, total) = match window(&path, digest) {
        Ok(read) => read,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return None,
        Err(err) => {
            tracing::warn!(%err, path = %path.display(), "a shared file could not be read");
            return None;
        }
    };

    // Say what was cut and where the rest is.
    let header = if total > EXCERPT_MAX_BYTES {
        let end = match digest {
            Digest::Tail => "last",
            _ => "first",
        };
        format!(
            "{rel} ({total} bytes; {end} {EXCERPT_MAX_BYTES} shown, `fs_read` it for the rest):"
        )
    } else {
        format!("{rel}:")
    };

    Some(format!("{header}\n{}", text.trim_end()))
}

/// How much of `STATUS.md` the board reads (Phase 17): more than the per-request
/// excerpt, still bounded.
pub const BOARD_MAX_BYTES: u64 = 64 * 1024;

/// The board file's absolute path and text (PLAN 7.2, row 3), or `None` when it
/// is missing or unreadable.
pub fn status(root: &Path) -> Option<(PathBuf, String)> {
    let path = inside(root, STATUS_FILE)?;

    let mut file = match fs::File::open(&path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return None,
        Err(err) => {
            tracing::warn!(%err, path = %path.display(), "the board file could not be read");
            return None;
        }
    };

    let mut bytes = Vec::new();
    if let Err(err) = std::io::Read::take(&mut file, BOARD_MAX_BYTES).read_to_end(&mut bytes) {
        tracing::warn!(%err, path = %path.display(), "the board file could not be read");
        return None;
    }

    Some((path, String::from_utf8_lossy(&bytes).into_owned()))
}

/// Reads at most [`EXCERPT_MAX_BYTES`] from one end of a file by seeking, and
/// returns the file's real length. Decoded lossily; a tail drops its partial
/// first line.
fn window(path: &Path, digest: Digest) -> io::Result<(String, u64)> {
    let mut file = fs::File::open(path)?;
    let total = file.metadata()?.len();

    if total <= EXCERPT_MAX_BYTES {
        let mut bytes = Vec::with_capacity(total as usize);
        file.read_to_end(&mut bytes)?;
        return Ok((String::from_utf8_lossy(&bytes).into_owned(), total));
    }

    let mut bytes = vec![0u8; EXCERPT_MAX_BYTES as usize];
    if digest == Digest::Tail {
        file.seek(SeekFrom::End(-(EXCERPT_MAX_BYTES as i64)))?;
    }
    file.read_exact(&mut bytes)?;

    let text = String::from_utf8_lossy(&bytes);
    let trimmed = match digest {
        Digest::Tail => match text.find('\n') {
            Some(at) => text[at + 1..].to_owned(),
            None => text.into_owned(),
        },
        _ => text.into_owned(),
    };
    Ok((trimmed, total))
}

// ---------------------------------------------------------------------------
// Write
// ---------------------------------------------------------------------------

/// Creates whatever part of the convention is missing in `root`. Never
/// overwrites or deletes (existing files are reported as `kept`), and stops at
/// the first failure.
///
/// Files only: the command runs [`git::ensure`] on the project's execution host
/// (PLAN 7.11, 7.12) and folds it in with [`ScaffoldReport::versioned`].
pub fn scaffold(root: &Path) -> AppResult<ScaffoldReport> {
    let mut created = Vec::new();
    let mut kept = Vec::new();

    for slot in &CONVENTION {
        let rel_dir = slot.rel_dir();
        let dir = contained(root, &rel_dir)?;
        if !dir.is_dir() {
            fs::create_dir_all(&dir).map_err(|err| failed(&rel_dir, &err))?;
        }

        let rel_file = slot.rel_file();
        let file = contained(root, &rel_file)?;
        if file.exists() {
            kept.push(rel_file);
            continue;
        }

        // A seed may sit a level down (`inbox.triage/SKILL.md`).
        if let Some(parent) = file.parent() {
            if !parent.is_dir() {
                fs::create_dir_all(parent).map_err(|err| failed(&rel_file, &err))?;
            }
        }

        fs::write(&file, slot.seed).map_err(|err| failed(&rel_file, &err))?;
        created.push(rel_file);
    }

    // Project evals (PLAN 7.18): a directory, and nothing seeded in it.
    let rel_evals = format!("{CABINET_DIR}/{}/", crate::agent::decision::eval::EVALS_DIR);
    let evals = contained(root, &rel_evals)?;
    if evals.is_dir() {
        kept.push(rel_evals);
    } else {
        fs::create_dir_all(&evals).map_err(|err| failed(&rel_evals, &err))?;
        created.push(rel_evals);
    }

    tracing::info!(
        root = %root.display(),
        created = created.len(),
        kept = kept.len(),
        "shared workspace files scaffolded"
    );
    Ok(ScaffoldReport {
        root: root.display().to_string(),
        created,
        kept,
        // Measured only; the command completes it (see above).
        versioning: git::measure(root),
        initialized: false,
        problem: None,
    })
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// Resolves a convention path through [`path::resolve`], refusing one that a
/// symlink points outside the workspace.
fn contained(root: &Path, rel: &str) -> AppResult<PathBuf> {
    let resolved = path::resolve(root, rel).map_err(|err| AppError::WorkspaceScaffold {
        path: rel.to_owned(),
        reason: err.reason().to_owned(),
    })?;

    if !resolved.inside {
        return Err(AppError::WorkspaceScaffold {
            path: rel.to_owned(),
            reason: "that name already points outside the workspace".to_owned(),
        });
    }
    Ok(resolved.path)
}

/// [`contained`] for reads: an unresolvable slot is simply absent (reason
/// logged at debug).
fn inside(root: &Path, rel: &str) -> Option<PathBuf> {
    match contained(root, rel) {
        Ok(path) => Some(path),
        Err(err) => {
            tracing::debug!(%err, rel, "a shared path does not resolve inside the workspace");
            None
        }
    }
}

/// An `io::Error` from someone else's folder, as something they can act on.
fn failed(rel: &str, err: &io::Error) -> AppError {
    tracing::warn!(%err, rel, "could not create a shared workspace file");

    AppError::WorkspaceScaffold {
        path: rel.to_owned(),
        reason: match err.kind() {
            io::ErrorKind::PermissionDenied => "the folder is not writable",
            io::ErrorKind::NotFound => "the workspace folder is not there",
            io::ErrorKind::AlreadyExists => "something else of that name is already there",
            _ => "it could not be created",
        }
        .to_owned(),
    }
}

#[cfg(test)]
mod tests;
