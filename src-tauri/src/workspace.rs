//! The shared-workspace convention (PLAN 7.3, Phases 11 and 13; `COS.md`
//! *Memory*).
//!
//! Four directories inside [`CABINET_DIR`] — `briefs/`, `status/`,
//! `artefacts/`, `decisions/` — and two files that carry state rather than
//! documentation: [`STATUS_FILE`] and [`DECISIONS_FILE`]. Phase 13
//! adds a fifth, `skills/`, for the runbooks that are about *this* project
//! rather than about the machine; it is here rather than in
//! [`skills`](crate::skills) because it is one more directory of the same
//! convention, laid down by the same button, and a second scaffolder would be
//! a second thing to keep in step. It reaches the model as the skill catalog
//! rather than through [`digest`], which is the one difference and is written
//! on the slot.
//!
//! ## Why they are under one directory, and why that directory is `.aegis`
//!
//! Five directories appearing at the root of somebody's repository, beside
//! `src/` and `docs/`, is five things they did not ask for. They go under one.
//!
//! The name is the tool's because the layer is: this is the **cabinet**
//! (PLAN 7.2, *Cabinet and constitution*) — in-flight work, rewritten every
//! turn, the harness's working surface over a project. What the project *is*
//! lives beside it in [`world`](crate::world), at the root, unprefixed and
//! first-class, because that half is not the tool's at all.
//!
//! The leading dot is not a hiding place, and it is worth being exact about
//! what PLAN 7.1 forbids: *a second hidden agent-memory filesystem that
//! **bypasses the workspace and the policy matrix***. Nothing here does. These
//! are ordinary files under the workspace root, contained by the same
//! [`path`](crate::policy::path) resolution, reached by the same `fs_read` /
//! `fs_write` under the same approval gate, on the same audit log, and
//! committed with the repository like any other directory. There is no store
//! behind them and no privileged writer: `.aegis/skills/inbox.triage/SKILL.md`
//! is a file in someone's repository that a person can edit, `git log`, and
//! delete.
//!
//! What the dot does cost is worth writing down rather than discovering: a
//! macOS Finder opened by [`reveal`](crate::reveal) hides it until
//! ⌘⇧. is pressed, and `rg` skips it without `--hidden`. That is the trade the
//! single root entry was worth.
//!
//! Three operations, and they are deliberately three of the `COS.md` names:
//!
//! * **scaffold** — [`scaffold`] creates what is missing and never touches what
//!   is there. It is opt-in: a workspace is a folder someone already owns, and
//!   writing four directories into it because an app was pointed at it would be
//!   the wrong default.
//! * **read** — [`digest`] is retrieved at the start of every model request, so
//!   the current state reaches the model as *state* rather than as a chat
//!   history to be re-read.
//! * **write** — there is no tool here at all. A decision is filed by writing
//!   [`DECISIONS_FILE`] with `fs_write`, under the gate, on the audit log. The
//!   convention is the schema; [`PREAMBLE`] is the instruction.
//!
//! ## Why the digest is small on purpose
//!
//! The system prompt must stay a policy summary plus the facts of this session
//! (PLAN 7.1, *System prompt*): a digest that grew into runbooks would be
//! procedure paid for on every turn, which is exactly what the skill catalog
//! and its load-on-demand body exist to avoid. So the digest carries *state*,
//! capped, and never procedure. The two state files are excerpted to
//! [`EXCERPT_MAX_BYTES`] each; `.aegis/briefs/` and `.aegis/artefacts/` contribute their file
//! **names** only. Nothing here pastes a brief or an artefact into the
//! conversation — `COS.md` is explicit that inputs are paths, never paste, and
//! the model already has `fs_read` for the rest.

use std::fs;
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use serde::Serialize;
use ts_rs::TS;

use crate::error::{AppError, AppResult};
use crate::policy::path;
use crate::skills;

/// The one directory the whole cabinet lives under, at the workspace root.
///
/// See the module header for why it is one directory and why it carries the
/// tool's name. The paths below are written out in full rather than composed at
/// runtime — they are `&'static str`, they are matched and joined all over the
/// tree, and `concat!` cannot see through a `const`. What keeps them honest is
/// [`every_convention_path_is_under_the_cabinet`], which is the one test that
/// fails if this constant and the literals below ever disagree.
///
/// [`every_convention_path_is_under_the_cabinet`]: self#tests
pub const CABINET_DIR: &str = ".aegis";

/// Where a delegated brief is filed (`COS.md` *Handoff*; PLAN 7.3, Phase 15).
///
/// Named rather than spelled inline because two things now depend on it: the
/// scaffolder below, and the handoff bus, which writes one file here per brief
/// it hands out and refuses to invent the directory if it is not already there.
pub const BRIEFS_DIR: &str = ".aegis/briefs";

/// The state file a session reads first: what is true now.
pub const STATUS_FILE: &str = ".aegis/status/STATUS.md";

/// The ledger a decision is filed in, instead of in the transcript.
pub const DECISIONS_FILE: &str = ".aegis/decisions/DECISIONS.md";

/// Most bytes of one state file that reach the system message.
///
/// A cap on the excerpt, not a limit on the file: `STATUS.md` and
/// `DECISIONS.md` are read on every request, and a workspace that has been
/// running for a year must not silently start costing a context window per
/// turn. What is elided is named in the digest, with the path, so the model can
/// go and read the rest.
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
///
/// The table below is the one place the convention is written down in Rust:
/// [`layout`], [`scaffold`] and [`digest`] all walk it, so adding a directory
/// is one entry rather than three edits that can disagree.
struct Slot {
    /// The directory's own name, inside [`CABINET_DIR`]: `briefs`, `status`, …
    ///
    /// The bare name rather than the path from the root, because it is both
    /// halves of what this type is asked: [`Slot::rel_dir`] puts the cabinet in
    /// front of it, and [`strays`] looks for exactly this name at the *root*,
    /// where an earlier version of the convention left it.
    name: &'static str,
    /// The file [`scaffold`] seeds inside it.
    file: &'static str,
    /// What that file starts as. Written once, never rewritten.
    seed: &'static str,
    /// What the slot contributes to [`digest`], or `None` when it contributes
    /// nothing.
    ///
    /// `skills/` is the one that contributes nothing, and deliberately: its
    /// contents already reach the model as the skill *catalog*
    /// ([`skills::prompt_block`](crate::skills::prompt_block)), which says
    /// what each runbook is for rather than only what it is called. Listing
    /// the filenames a second time would be paying twice for less.
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

/// The convention, in the order it is shown and created.
///
/// Brief in, work, artefact out, decision recorded: the order a piece of work
/// actually moves through, which is also the order that reads best in a panel.
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
    // Phase 13. A workspace's own runbooks travel with the folder, which is
    // the whole reason the scope exists: "how *this* project is deployed" is
    // not a fact about the machine Aegis is installed on. Seeded with the
    // `inbox.triage` stub PLAN 7.3 asks for — file in, status and artefact
    // out — so the format has an example in the place people will look for
    // one.
    Slot {
        name: skills::LIBRARY_DIR,
        file: TRIAGE_FILE,
        seed: skills::TRIAGE_SEED,
        digest: None,
    },
];

/// The stub runbook seeded into a workspace, relative to `skills/`.
///
/// A skill is a directory holding a `SKILL.md`, so this slot's "file" is two
/// levels deep — which is why [`scaffold`] creates the seed's parent rather
/// than only the slot's directory.
const TRIAGE_FILE: &str = "inbox.triage/SKILL.md";

/// What the model is told once the convention is present.
///
/// Where each kind of thing lives, and the one rule that makes the convention
/// worth having — the decision goes in the file, not in the thread.
/// Deliberately not a runbook: how to triage an inbox or review a patch is a
/// skill ([`skills`](crate::skills)), loaded into the one turn that runs it.
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

/// One directory of the convention, as the UI sees it.
///
/// Both flags are measured on every read and never stored: a user can create
/// `decisions/` in a terminal, or delete it, and the panel has to be right
/// about a folder it does not own.
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
    ///
    /// Derived here rather than in the UI, so that "set up" means the same
    /// thing to the panel, to a test, and to whatever later phase asks.
    pub complete: bool,
    /// Convention directories found at the workspace *root*, from the layout
    /// this build no longer uses. Bare names: `briefs`, `decisions`, …
    ///
    /// The cabinet moved under [`CABINET_DIR`], and a folder set up before that
    /// still has its `.aegis/status/STATUS.md` where it always was — full of work the
    /// runtime has just stopped being able to see. So they are named, and that
    /// is all: nothing here moves a directory in somebody's repository. Picking
    /// a folder was never consent to rearrange it, and the same rule that keeps
    /// [`scaffold`] from overwriting a file keeps this from relocating one.
    ///
    /// Empty for every workspace that never had the old layout, which is the
    /// ordinary case and draws nothing.
    pub strays: Vec<String>,
}

/// What one scaffolding run did.
///
/// Two lists rather than a count: the point of the report is that the user can
/// see nothing of theirs was overwritten, and only naming what was kept says
/// that. Paths are relative to the workspace root, in the form the convention
/// is written in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct ScaffoldReport {
    /// The workspace root the paths are relative to.
    pub root: String,
    /// Files created by this run, in convention order.
    pub created: Vec<String>,
    /// Files that were already there and were left exactly as they were.
    pub kept: Vec<String>,
}

// ---------------------------------------------------------------------------
// Read
// ---------------------------------------------------------------------------

/// Which parts of the convention exist in `root` right now.
///
/// Never fails: a workspace that has been unmounted, or one the app cannot
/// read, reports everything absent, which is the truthful answer and the one
/// the panel can render. The failure that matters is the one [`scaffold`]
/// returns, where a user is waiting on an outcome.
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
        entries,
    }
}

/// Convention directories still sitting at the workspace root.
///
/// Keyed on the *seed file* rather than the directory, and that is the whole
/// care in it: `skills/` at the root of a repository is somebody's own folder
/// far more often than it is this convention, and a panel that told a Rust
/// project its `status/` was in the wrong place would be a panel people learn
/// to ignore. A root `.aegis/status/STATUS.md` or `.aegis/decisions/DECISIONS.md` is a much
/// narrower claim, and it is the one that is worth making.
fn strays(root: &Path) -> Vec<String> {
    CONVENTION
        .iter()
        .filter(|slot| {
            inside(root, &format!("{}/{}", slot.name, slot.file)).is_some_and(|file| file.is_file())
        })
        .map(|slot| slot.name.to_owned())
        .collect()
}

/// The shared state, as a block for the system message — the *read* path.
///
/// `None` when the workspace has none of the convention in it. A folder that
/// was never scaffolded is a plain workspace, and its sessions get exactly the
/// prompt they got before this phase: nothing is appended to nag the user into
/// a convention they did not ask for.
///
/// Rebuilt per request rather than once per session. That is a superset of
/// "retrieve at session start" and it is what makes the loop honest: when a
/// turn writes `DECISIONS.md`, the next round of that same turn already sees
/// it. The cost is two capped reads and two directory listings.
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

    // Named, not silently truncated: the model is told how much it is not
    // seeing and where the rest is, which is the difference between an excerpt
    // and a file that quietly lies about its own length.
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

/// How much of `.aegis/status/STATUS.md` the board reads (PLAN 7.3, Phase 17).
///
/// Larger than [`EXCERPT_MAX_BYTES`] and for a different reason. The excerpt is
/// paid for on every model request, so it is small; the board is read when
/// somebody opens it, and what they are opening it for is the whole of what is
/// true right now. It is still bounded, because the file belongs to the user
/// and nothing stops them pointing Aegis at a folder whose `STATUS.md` is a
/// log somebody has been appending to for a year.
pub const BOARD_MAX_BYTES: u64 = 64 * 1024;

/// The board file and its text, for the structured read of `/status`
/// (PLAN 7.2, row 3).
///
/// `None` when the convention has not been laid down, when the file was
/// deleted, or when it cannot be read — three states the board draws the same
/// way, because the answer to all of them is the same: there is no board in
/// this folder yet, and the button that makes one is in the sidebar.
///
/// The path comes back beside the text so the panel can name the file it is
/// showing. It is the absolute one: the board is the one place a person is
/// invited to go and edit the file by hand.
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

/// Reads at most [`EXCERPT_MAX_BYTES`] from one end of a file.
///
/// Seeking rather than reading the file and slicing it: these are read on every
/// request, and a workspace whose ledger has grown to megabytes must not pay
/// for all of it to produce two kilobytes. The returned length is the file's
/// real one, so the caller can say what it is not showing.
///
/// Bytes are decoded lossily, and a partial first line is dropped from a tail:
/// a window into a text file lands wherever the cap lands — mid-rune and
/// mid-sentence — and a mangled leading line reads as corruption to whoever
/// sees the prompt.
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
        // A tail starts mid-line; drop the fragment rather than show half a
        // decision as though it were a whole one.
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

/// Creates whatever part of the convention is missing in `root`.
///
/// Never overwrites and never deletes. A file that is already there is left
/// byte for byte as it was and reported under `kept` — the seeds are a starting
/// point, and a `STATUS.md` somebody has been maintaining for a month is worth
/// more than the template it grew out of. Re-running this on a complete
/// workspace is therefore a no-op that returns the same report every time.
///
/// Stops at the first failure rather than pressing on. A workspace where three
/// of four directories appeared is a state the user then has to reason about; a
/// clear error naming the one that failed is not.
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

        // A slot's seed may sit a level below its directory — a skill is a
        // folder holding a `SKILL.md` — so the file's own parent is created
        // rather than only the slot's own directory.
        if let Some(parent) = file.parent() {
            if !parent.is_dir() {
                fs::create_dir_all(parent).map_err(|err| failed(&rel_file, &err))?;
            }
        }

        fs::write(&file, slot.seed).map_err(|err| failed(&rel_file, &err))?;
        created.push(rel_file);
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
    })
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// Resolves a convention path against the workspace, or explains why not.
///
/// Goes through [`path::resolve`] rather than joining, for the reason that
/// module exists: `briefs` may already be a symlink someone pointed at another
/// disk, and neither reading a file into a model request nor writing a seed
/// into it should happen outside the folder the user chose. Nothing in
/// [`CONVENTION`] is user input, so a refusal here is a fact about the
/// workspace rather than about an argument.
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

/// [`contained`] for the read paths, where a refusal is not worth an error.
///
/// [`layout`] and [`digest`] run on every panel render and every model request;
/// the honest answer for a slot that cannot be resolved is "it is not there",
/// and the log carries the reason for whoever is debugging it.
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
mod tests {
    use super::*;

    use tempfile::TempDir;

    /// A canonical, empty workspace folder.
    fn workspace() -> (TempDir, PathBuf) {
        let dir = TempDir::new().expect("temp dir");
        let root = dir.path().join("work");
        fs::create_dir_all(&root).expect("workspace dir");
        let root = dunce::canonicalize(&root).expect("canonical workspace");
        (dir, root)
    }

    #[test]
    fn an_untouched_workspace_has_none_of_the_convention() {
        let (_dir, root) = workspace();
        let layout = layout(&root);

        assert_eq!(layout.entries.len(), CONVENTION.len());
        assert!(layout
            .entries
            .iter()
            .all(|entry| !entry.dir_exists && !entry.file_exists));
        assert!(!layout.complete);
    }

    /// [`CABINET_DIR`] and the three spelled-out constants have to agree, and
    /// nothing but this test makes them: `concat!` cannot see through a `const`,
    /// so the literals are written by hand and checked here.
    #[test]
    fn every_convention_path_is_under_the_cabinet() {
        let prefix = format!("{CABINET_DIR}/");
        for path in [BRIEFS_DIR, STATUS_FILE, DECISIONS_FILE] {
            assert!(
                path.starts_with(&prefix),
                "{path} is not under {CABINET_DIR}"
            );
        }
        for slot in &CONVENTION {
            assert!(slot.rel_dir().starts_with(&prefix), "{}", slot.name);
            assert!(slot.rel_file().starts_with(&prefix), "{}", slot.name);
        }

        // And the two that are named twice do resolve to the same slot, which is
        // what a `format!` in one place and a literal in another can silently
        // stop doing.
        assert_eq!(BRIEFS_DIR, CONVENTION[0].rel_dir());
        assert_eq!(STATUS_FILE, CONVENTION[1].rel_file());
        assert_eq!(DECISIONS_FILE, CONVENTION[3].rel_file());
    }

    /// The cabinet moved, and a folder set up before it did still has work in
    /// the old place. It is named, and it is not touched — picking a folder was
    /// never consent to rearrange it.
    #[test]
    fn the_old_layout_at_the_root_is_reported_and_left_alone() {
        let (_dir, root) = workspace();
        assert!(layout(&root).strays.is_empty(), "an empty folder has none");

        fs::create_dir_all(root.join("decisions")).expect("the old decisions dir");
        fs::write(root.join("decisions/DECISIONS.md"), "ours, from before\n").expect("write");

        let found = layout(&root);
        assert_eq!(found.strays, vec!["decisions"]);
        assert!(!found.complete, "and the convention is still not laid down");

        // Scaffolding beside it creates the new one and does not move, read or
        // delete the old.
        scaffold(&root).expect("scaffolded");
        assert_eq!(
            fs::read_to_string(root.join("decisions/DECISIONS.md")).expect("read"),
            "ours, from before\n",
            "the file somebody wrote is theirs"
        );
        assert_eq!(
            layout(&root).strays,
            vec!["decisions"],
            "and still reported"
        );
    }

    /// A bare directory of that name is not a claim. Plenty of repositories have
    /// a `skills/` or a `status/` at their root that has nothing to do with this
    /// convention, and a panel that told them they were in the wrong place is a
    /// panel people learn to ignore.
    #[test]
    fn a_directory_that_merely_shares_a_name_is_not_the_old_layout() {
        let (_dir, root) = workspace();
        fs::create_dir_all(root.join("skills/whatever")).expect("somebody's own folder");
        fs::create_dir_all(root.join("status")).expect("and another");
        fs::write(root.join("status/notes.txt"), "unrelated").expect("write");

        assert!(
            layout(&root).strays.is_empty(),
            "only the seed file's own name is a claim worth making"
        );
    }

    /// The prompt of a folder nobody opted in is exactly the prompt it was
    /// before this phase.
    #[test]
    fn a_workspace_without_the_convention_contributes_nothing_to_a_prompt() {
        let (_dir, root) = workspace();

        assert_eq!(digest(&root), None);
    }

    #[test]
    fn scaffolding_creates_every_directory_of_the_convention_and_its_file() {
        let (_dir, root) = workspace();
        let report = scaffold(&root).expect("scaffolded");

        assert_eq!(
            report.created,
            vec![
                ".aegis/briefs/README.md",
                ".aegis/status/STATUS.md",
                ".aegis/artefacts/README.md",
                ".aegis/decisions/DECISIONS.md",
                ".aegis/skills/inbox.triage/SKILL.md",
            ]
        );
        assert!(report.kept.is_empty());
        assert!(layout(&root).complete);
        assert!(root.join(STATUS_FILE).is_file());
        assert!(root.join(DECISIONS_FILE).is_file());
    }

    /// The workspace stub of PLAN 7.3, Phase 13, laid down by the same button
    /// as the rest of the convention — and found by the runner as a workspace
    /// skill, which is the only thing that makes seeding it worth anything.
    #[test]
    fn the_seeded_workspace_runbook_is_one_the_runner_can_find_and_read() {
        let (_dir, root) = workspace();
        scaffold(&root).expect("scaffolded");

        let found = skills::catalog(&root.join("nothing-here"), Some(&root));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "inbox.triage");
        assert_eq!(found[0].scope, skills::SkillScope::Workspace);
        assert!(found[0].runnable(), "{:?}", found[0].problem);
    }

    /// `skills/` is in the convention and not in the digest: the catalog
    /// already tells the model what each runbook is for, and listing the
    /// filenames again would cost a line of every request to say less.
    #[test]
    fn the_skills_directory_contributes_nothing_to_the_digest() {
        let (_dir, root) = workspace();
        scaffold(&root).expect("scaffolded");

        let digest = digest(&root).expect("a digest");
        assert!(digest.contains(".aegis/briefs/"), "{digest}");
        assert!(!digest.contains("skills/"), "{digest}");
        assert!(!digest.contains("inbox.triage"), "{digest}");
    }

    /// The point of the report: re-running it is safe, and says so.
    #[test]
    fn scaffolding_twice_creates_nothing_the_second_time() {
        let (_dir, root) = workspace();
        let first = scaffold(&root).expect("scaffolded");
        let second = scaffold(&root).expect("scaffolded again");

        assert_eq!(second.created, Vec::<String>::new());
        assert_eq!(second.kept, first.created);
    }

    /// The one thing scaffolding must never do to somebody's own folder.
    #[test]
    fn scaffolding_never_overwrites_what_is_already_there() {
        let (_dir, root) = workspace();
        fs::create_dir_all(root.join(".aegis/decisions")).expect("decisions dir");
        fs::write(root.join(DECISIONS_FILE), "# Decisions\n\nours, kept\n").expect("write");

        let report = scaffold(&root).expect("scaffolded");

        assert_eq!(report.kept, vec![DECISIONS_FILE]);
        assert_eq!(
            fs::read_to_string(root.join(DECISIONS_FILE)).expect("read"),
            "# Decisions\n\nours, kept\n"
        );
    }

    #[test]
    fn the_digest_states_the_write_rule_and_names_both_state_files() {
        let (_dir, root) = workspace();
        scaffold(&root).expect("scaffolded");
        let digest = digest(&root).expect("a scaffolded workspace has a digest");

        assert!(digest.contains(STATUS_FILE), "{digest}");
        assert!(digest.contains(DECISIONS_FILE), "{digest}");
        assert!(
            digest.contains("not in this conversation"),
            "the write rule is stated: {digest}"
        );
    }

    /// `COS.md`: inputs are paths, never paste. A brief's content stays on
    /// disk; only its name is worth a prompt.
    #[test]
    fn briefs_and_artefacts_contribute_names_and_never_content() {
        let (_dir, root) = workspace();
        scaffold(&root).expect("scaffolded");
        fs::write(root.join(".aegis/briefs/intake.md"), "SECRET BRIEF BODY").expect("write");

        let digest = digest(&root).expect("a digest");

        assert!(
            digest.contains(".aegis/briefs/: README.md, intake.md"),
            "{digest}"
        );
        assert!(!digest.contains("SECRET BRIEF BODY"), "{digest}");
    }

    #[test]
    fn an_empty_shared_directory_says_so_rather_than_vanishing() {
        let (_dir, root) = workspace();
        fs::create_dir_all(root.join(".aegis/artefacts")).expect("artefacts dir");

        let digest = digest(&root).expect("one directory is enough for a digest");

        assert!(digest.contains(".aegis/artefacts/: (empty)"), "{digest}");
    }

    /// A ledger is appended to, so the recent end is the useful one — and the
    /// model is told what it is not being shown.
    #[test]
    fn a_long_ledger_is_tailed_and_the_elision_is_named() {
        let (_dir, root) = workspace();
        scaffold(&root).expect("scaffolded");

        let filler = "x".repeat(EXCERPT_MAX_BYTES as usize * 2);
        fs::write(
            root.join(DECISIONS_FILE),
            format!("# Decisions\n{filler}\nthe most recent decision\n"),
        )
        .expect("write");

        let digest = digest(&root).expect("a digest");

        assert!(digest.contains("the most recent decision"), "{digest}");
        assert!(!digest.contains("# Decisions"), "the head is dropped");
        assert!(
            digest.contains("fs_read"),
            "the rest is pointed at: {digest}"
        );
    }

    /// A board is rewritten in place, so the top is the current state.
    #[test]
    fn a_long_status_board_is_headed() {
        let (_dir, root) = workspace();
        scaffold(&root).expect("scaffolded");

        let filler = "y".repeat(EXCERPT_MAX_BYTES as usize * 2);
        fs::write(
            root.join(STATUS_FILE),
            format!("# Status\nwhat is true now\n{filler}\ntrailing noise\n"),
        )
        .expect("write");

        let digest = digest(&root).expect("a digest");

        assert!(digest.contains("what is true now"), "{digest}");
        assert!(!digest.contains("trailing noise"), "the tail is dropped");
    }

    /// Neither cap may be exceeded by a file that has grown without bound.
    #[test]
    fn the_digest_stays_bounded_however_large_the_files_get() {
        let (_dir, root) = workspace();
        scaffold(&root).expect("scaffolded");

        let huge = "z".repeat(EXCERPT_MAX_BYTES as usize * 64);
        fs::write(root.join(STATUS_FILE), &huge).expect("write");
        fs::write(root.join(DECISIONS_FILE), &huge).expect("write");
        for n in 0..40 {
            fs::write(root.join(format!(".aegis/briefs/{n}.md")), "b").expect("write");
        }

        let digest = digest(&root).expect("a digest");

        assert!(
            digest.len() < PREAMBLE.len() + 6 * 1024,
            "the digest grew to {} bytes",
            digest.len()
        );
        assert!(digest.contains("and 29 more"), "{digest}");
    }

    /// A file that is not valid UTF-8 is a thing a workspace can contain. It
    /// must degrade to replacement characters rather than take the turn down.
    #[test]
    fn a_state_file_that_is_not_utf8_still_produces_a_digest() {
        let (_dir, root) = workspace();
        scaffold(&root).expect("scaffolded");
        fs::write(root.join(STATUS_FILE), [0xff, 0xfe, b'o', b'k']).expect("write");

        let digest = digest(&root).expect("a digest");

        assert!(digest.contains("ok"), "{digest}");
    }
}
