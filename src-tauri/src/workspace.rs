//! The shared-workspace convention (PLAN 7.3, Phases 11 and 13; `COS.md`
//! *Memory*).
//!
//! Four directories inside the folder the user picked — `briefs/`, `status/`,
//! `artefacts/`, `decisions/` — and two files that carry state rather than
//! documentation: `status/STATUS.md` and `decisions/DECISIONS.md`. Phase 13
//! adds a fifth, `skills/`, for the runbooks that are about *this* project
//! rather than about the machine; it is here rather than in
//! [`skills`](crate::skills) because it is one more directory of the same
//! convention, laid down by the same button, and a second scaffolder would be
//! a second thing to keep in step. It reaches the model as the skill catalog
//! rather than through [`digest`], which is the one difference and is written
//! on the slot. That is the whole of it. No new store, no hidden directory, no second filesystem beside
//! the workspace (PLAN 7.1, *Workspace*): these are ordinary files in the
//! user's own folder, which is what makes them editable by a human, visible to
//! `git`, and reachable by the same `fs_read` / `fs_write` tools under the same
//! approval gate as everything else.
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
//!   `decisions/DECISIONS.md` with `fs_write`, under the gate, on the audit
//!   log. The convention is the schema; [`PREAMBLE`] is the instruction.
//!
//! Every path in [`CONVENTION`] is a path the ordinary tools can reach, and
//! nothing here is privileged: `skills/inbox.triage/SKILL.md` is a file in
//! someone's repository that a person can edit, `git log`, and delete.
//!
//! ## Why the digest is small on purpose
//!
//! The system prompt must stay a policy summary plus the facts of this session
//! (PLAN 7.1, *System prompt*): a digest that grew into runbooks would be
//! procedure paid for on every turn, which is exactly what the skill catalog
//! and its load-on-demand body exist to avoid. So the digest carries *state*,
//! capped, and never procedure. The two state files are excerpted to
//! [`EXCERPT_MAX_BYTES`] each; `briefs/` and `artefacts/` contribute their file
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

/// The state file a session reads first: what is true now.
pub const STATUS_FILE: &str = "status/STATUS.md";

/// The ledger a decision is filed in, instead of in the transcript.
pub const DECISIONS_FILE: &str = "decisions/DECISIONS.md";

/// Most bytes of one state file that reach the system message.
///
/// A cap on the excerpt, not a limit on the file: `STATUS.md` and
/// `DECISIONS.md` are read on every request, and a workspace that has been
/// running for a year must not silently start costing a context window per
/// turn. What is elided is named in the digest, with the path, so the model can
/// go and read the rest.
pub const EXCERPT_MAX_BYTES: u64 = 2 * 1024;

/// Most file names listed for `briefs/` and `artefacts/`.
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
    /// Directory name, relative to the workspace root.
    dir: &'static str,
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
    /// The seed file's path relative to the workspace root, with a `/`
    /// separator — the form the convention is written in, and one
    /// [`path::resolve`] accepts on every platform.
    fn rel_file(&self) -> String {
        format!("{}/{}", self.dir, self.file)
    }
}

/// The convention, in the order it is shown and created.
///
/// Brief in, work, artefact out, decision recorded: the order a piece of work
/// actually moves through, which is also the order that reads best in a panel.
const CONVENTION: [Slot; 5] = [
    Slot {
        dir: "briefs",
        file: "README.md",
        seed: BRIEFS_SEED,
        digest: Some(Digest::Listing),
    },
    Slot {
        dir: "status",
        file: "STATUS.md",
        seed: STATUS_SEED,
        digest: Some(Digest::Head),
    },
    Slot {
        dir: "artefacts",
        file: "README.md",
        seed: ARTEFACTS_SEED,
        digest: Some(Digest::Listing),
    },
    Slot {
        dir: "decisions",
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
        dir: skills::LIBRARY_DIR,
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
This workspace keeps its shared memory in files. Delegated work is briefed in \
`briefs/`, what is true right now is in `status/STATUS.md`, anything produced \
goes in `artefacts/`, and decisions are recorded in `decisions/DECISIONS.md`. \
A decision or a status belongs in its file, not in this conversation: the \
conversation is forgotten, the file is not. `fs_read` a file before you change \
it and write it back whole, because `fs_write` replaces. What follows is the \
state as of the start of this reply.";

const BRIEFS_SEED: &str = r#"# briefs/

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
# artefacts/

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
    /// Directory name, relative to the workspace root: `briefs`, `status`, …
    pub dir: String,
    /// The seed file inside it, relative to the root: `status/STATUS.md`, …
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
            let rel_file = slot.rel_file();
            WorkspaceEntry {
                dir_exists: inside(root, slot.dir).is_some_and(|dir| dir.is_dir()),
                file_exists: inside(root, &rel_file).is_some_and(|file| file.is_file()),
                dir: slot.dir.to_owned(),
                file: rel_file,
            }
        })
        .collect();

    WorkspaceLayout {
        root: root.display().to_string(),
        complete: entries
            .iter()
            .all(|entry| entry.dir_exists && entry.file_exists),
        entries,
    }
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

/// `briefs/: intake.md, q3-plan.md` — names only, never content.
fn listing(root: &Path, slot: &Slot) -> Option<String> {
    let dir = inside(root, slot.dir)?;
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

    let mut line = format!("{}/: ", slot.dir);
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

/// `status/STATUS.md:` and the capped content beneath it.
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
        let dir = contained(root, slot.dir)?;
        if !dir.is_dir() {
            fs::create_dir_all(&dir).map_err(|err| failed(slot.dir, &err))?;
        }

        let rel_file = slot.rel_file();
        let file = contained(root, &rel_file)?;
        if file.exists() {
            kept.push(rel_file);
            continue;
        }

        // A slot's seed may sit a level below its directory — a skill is a
        // folder holding a `SKILL.md` — so the file's own parent is created
        // rather than only `slot.dir`.
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
                "briefs/README.md",
                "status/STATUS.md",
                "artefacts/README.md",
                "decisions/DECISIONS.md",
                "skills/inbox.triage/SKILL.md",
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
        assert!(digest.contains("briefs/"), "{digest}");
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
        fs::create_dir_all(root.join("decisions")).expect("decisions dir");
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
        fs::write(root.join("briefs/intake.md"), "SECRET BRIEF BODY").expect("write");

        let digest = digest(&root).expect("a digest");

        assert!(digest.contains("briefs/: README.md, intake.md"), "{digest}");
        assert!(!digest.contains("SECRET BRIEF BODY"), "{digest}");
    }

    #[test]
    fn an_empty_shared_directory_says_so_rather_than_vanishing() {
        let (_dir, root) = workspace();
        fs::create_dir_all(root.join("artefacts")).expect("artefacts dir");

        let digest = digest(&root).expect("one directory is enough for a digest");

        assert!(digest.contains("artefacts/: (empty)"), "{digest}");
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
            fs::write(root.join(format!("briefs/{n}.md")), "b").expect("write");
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
