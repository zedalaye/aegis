//! The constitution of a workspace: `world/` (PLAN 7.2; `COS.md` *Work*).
//!
//! What the project *is*, at the root beside the `.aegis/` cabinet, because it
//! belongs to the project rather than to Aegis.
//!
//! * **Opt-in.** Nothing creates it; a world exists once a constitution file
//!   does. The absence is still named in the system message (PLAN 7.17), so a
//!   first session can tell `world/` from `.aegis/` without a runbook.
//! * **Specialists never write it.** A delegated run's write is refused
//!   ([`matrix`](crate::policy::matrix)); otherwise it is asked at high risk with
//!   a `world/`-only grant. The frame matches the run ([`FRAME_DELEGATED`] vs
//!   [`FRAME_CABINET`]), so the model does not refuse what the gate would ask.
//! * **Sources are hashed, not `world/`.** Drift in [`SOURCES_FILE`]'s declared
//!   artefacts is measured when a brief launches ([`blocking`]) and when the
//!   panel opens ([`status`]).
//! * **An in-step source is not re-read.** A read is refused; a moved source
//!   reads normally so it can be re-perceived.
//!
//! The model gets [`block`] — a short frame plus status — never `essence.md`
//! itself (PLAN 7.1).

use std::fmt::Write as _;
use std::fs;
use std::io::{self, Read as _};
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use sha2::{Digest as _, Sha256};
use ts_rs::TS;

use crate::policy::path;

/// The constitution's directory, relative to the workspace root.
pub const WORLD_DIR: &str = "world";

/// Where the source artefacts a world was perceived from are declared.
pub const SOURCES_FILE: &str = "world/sources.yml";

/// Chunk size for hashing a declared source, which may be larger than memory.
const HASH_CHUNK: usize = 64 * 1024;

/// Most sources one `sources.yml` may declare; the list reaches the system
/// message.
const SOURCES_MAX: usize = 32;

/// One file of the constitution, and what it is for.
struct Leaf {
    /// The file's name inside `world/`.
    file: &'static str,
    /// What it holds, in the words the panel and the frame both use.
    what: &'static str,
}

/// The constitution, in reading order. Every file is optional.
const CONSTITUTION: [Leaf; 5] = [
    Leaf {
        file: "essence.md",
        what: "what this is, and what it is for",
    },
    Leaf {
        file: "schema.md",
        what: "the shapes it is made of, as they were perceived",
    },
    Leaf {
        file: "behaviours.md",
        what: "how it behaves, including the sins a source showed, each with its perimeter",
    },
    Leaf {
        file: "oracle.md",
        what: "how a new instance is known to be right",
    },
    Leaf {
        file: "decisions.md",
        what: "the decisions that moved the essence, which are not the operational ones",
    },
];

/// What every session on a world is told — injected rather than a skill, which
/// could be skipped (PLAN 7.2). No procedure.
const FRAME: &str = "\
This workspace has a world. `world/` is what this project *is*, and for this \
session it is the constitution: read it before you plan anything, starting with \
`world/essence.md`, and take what it says as given rather than re-deriving it \
from the code, from a transcript, or from a dump. A turn that starts by \
exploring a project the world already describes has spent its budget on \
something that was already paid for.

You do not reopen the declared sources below to understand the project. They \
have been perceived and what they said is in `world/`; a read of one that has \
not changed is refused rather than asked about.";

/// The writing half of the frame for a delegated run, where the gate refuses
/// `world/` writes ([`matrix`](crate::policy::matrix)).
const FRAME_DELEGATED: &str = "\
You do not write `world/`. Amending the essence is a human decision, and this \
is a brief: a write there is refused outright, not put to anybody. If the work \
you were given cannot be done without changing what this thing is, that is the \
answer — stop, say in one sentence which line of the essence would have to \
move and why, and return `needs_you`. That is an écart, and it is worth more \
than a plausible instance built on a world nobody agreed to change.";

/// The same half for an attended session, where writes are asked, not refused —
/// otherwise the prompt would block [`world.draft`](crate::skills::DRAFT_SKILL).
const FRAME_CABINET: &str = "\
`world/` is not yours to change on your own initiative. Amending the essence is \
the operator's decision, so do not rewrite it to make a task fit: if what you \
were asked for cannot be done without changing what this thing is, say in one \
sentence which line would have to move, and let them decide. That is an écart, \
and it is worth more than a plausible instance built on a world nobody agreed \
to change.

When they *do* ask you to found or amend it, write it — that is them deciding, \
and `world.draft` is the runbook for it. Every write into `world/` is put to \
them for approval on its own, at high risk, so they see each file before it \
lands; they may allow the rest of the session in one answer, which is theirs to \
offer and not yours to ask for twice.";

/// Named when there is no world, so a first session is not left to guess
/// (PLAN 7.17). Vocabulary only: no file checklist, no procedure. Bound by
/// [`the_absent_frame_stays_small`].
///
/// [`the_absent_frame_stays_small`]: self#tests
const FRAME_ABSENT: &str = "\
This workspace has no world. `world/` at the root is the constitution (what \
the project *is*). `.aegis/` is the cabinet — in-flight work. To found a \
world, load `world.draft` if you hold it; otherwise say so. Do not invent an \
essence.";

/// The same layers, without pointing a brief at a runbook it cannot finish.
const FRAME_ABSENT_DELEGATED: &str = "\
This workspace has no world. `world/` at the root is the constitution (what \
the project *is*). `.aegis/` is the cabinet — in-flight work. A brief does \
not found a world.";

// ---------------------------------------------------------------------------
// IPC payloads
// ---------------------------------------------------------------------------

/// What is true of one declared source right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum SourceState {
    /// There, and byte for byte what the world recorded.
    InStep,
    /// There, and not what the world recorded. It has not been perceived yet.
    Drifted,
    /// Declared, and not on disk.
    Missing,
    /// Declared with no digest, so nothing has ever been perceived from it.
    Unrecorded,
}

impl SourceState {
    /// Whether this needs acting on before routing (`COS.md` *Loop*).
    pub const fn is_drift(self) -> bool {
        matches!(self, Self::Drifted | Self::Missing | Self::Unrecorded)
    }

    /// How the state is named to a person and to the model.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InStep => "in step",
            Self::Drifted => "drifted",
            Self::Missing => "not there",
            Self::Unrecorded => "never recorded",
        }
    }
}

/// One file of the constitution, as the panel sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct WorldFile {
    /// The path relative to the workspace root: `world/essence.md`.
    pub file: String,
    /// What it is for, in one line.
    pub what: String,
    /// Whether it is there right now.
    pub exists: bool,
}

/// One declared source, as the panel sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct WorldSource {
    /// The path as `sources.yml` declares it, relative to the workspace root.
    pub path: String,
    /// What is true of it right now, measured by reading it.
    pub state: SourceState,
}

/// The world's state in one workspace, measured on every call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct WorldStatus {
    /// Whether there is a world in this workspace at all.
    pub present: bool,
    /// Every file of the constitution, present or not, in reading order.
    pub files: Vec<WorldFile>,
    /// What `world/sources.yml` declares, and what is true of each one.
    pub sources: Vec<WorldSource>,
    /// Whether any declared source has moved since it was perceived.
    pub drifted: bool,
    /// Why `sources.yml` could not be read as written, when it could not.
    pub problem: Option<String>,
}

// ---------------------------------------------------------------------------
// Reading the world
// ---------------------------------------------------------------------------

/// One declared source artefact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// The path as declared, relative to the workspace root and written with
    /// `/`, which is the form it is shown and compared in.
    pub path: String,
    /// Where that resolves to on this machine.
    resolved: PathBuf,
    /// The length it was recorded at, when one was recorded.
    bytes: Option<u64>,
    /// The digest it was recorded at, lower-case hex, when one was recorded.
    sha256: Option<String>,
}

/// The constitution as it is on disk right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct World {
    /// The files of [`CONSTITUTION`] that are actually there, in order.
    held: Vec<&'static str>,
    /// What [`SOURCES_FILE`] declares, capped at [`SOURCES_MAX`].
    sources: Vec<Source>,
    /// Why that file could not be read as written, when it could not.
    problem: Option<String>,
}

/// The world in `root`: `world/` holding at least one [`CONSTITUTION`] file (a
/// lone `sources.yml` is not a world). Never fails; unreadable parts are absent.
pub fn read(root: &Path) -> Option<World> {
    let dir = inside(root, WORLD_DIR)?;
    if !dir.is_dir() {
        return None;
    }

    let held: Vec<&'static str> = CONSTITUTION
        .iter()
        .filter(|leaf| dir.join(leaf.file).is_file())
        .map(|leaf| leaf.file)
        .collect();
    if held.is_empty() {
        return None;
    }

    let (sources, problem) = sources(root);
    Some(World {
        held,
        sources,
        problem,
    })
}

impl World {
    /// What is true of every declared source, hashing as needed. Expensive:
    /// never called on the way into a model request.
    pub fn measure(&self) -> Vec<(&Source, SourceState)> {
        self.sources
            .iter()
            .map(|source| (source, state_of(source)))
            .collect()
    }

    /// The declared sources that have moved since the world recorded them.
    pub fn drifted(&self) -> Vec<(&Source, SourceState)> {
        self.measure()
            .into_iter()
            .filter(|(_, state)| state.is_drift())
            .collect()
    }

    /// The declared source at `target`, when there is one.
    fn declaring(&self, target: &Path) -> Option<&Source> {
        self.sources.iter().find(|source| source.resolved == target)
    }
}

/// What is true of one declared source, read in full.
fn state_of(source: &Source) -> SourceState {
    let Some(recorded) = source.sha256.as_deref() else {
        return if source.resolved.exists() {
            SourceState::Unrecorded
        } else {
            SourceState::Missing
        };
    };

    let Ok(meta) = fs::metadata(&source.resolved) else {
        return SourceState::Missing;
    };
    if !meta.is_file() {
        // Only files can be hashed; a directory cannot be shown in step.
        return SourceState::Drifted;
    }
    // A length change settles it without hashing.
    if source.bytes.is_some_and(|len| len != meta.len()) {
        return SourceState::Drifted;
    }

    match hash(&source.resolved) {
        Ok(found) if found.eq_ignore_ascii_case(recorded) => SourceState::InStep,
        Ok(_) => SourceState::Drifted,
        Err(err) => {
            tracing::warn!(
                %err,
                path = %source.resolved.display(),
                "a declared source could not be read to check it"
            );
            SourceState::Drifted
        }
    }
}

/// The SHA-256 of a file, lower-case hex, read in chunks.
fn hash(path: &Path) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; HASH_CHUNK];

    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(hasher
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        }))
}

// ---------------------------------------------------------------------------
// `world/sources.yml`
// ---------------------------------------------------------------------------

/// What [`SOURCES_FILE`] declares, and the first thing wrong with it. Parsed by
/// hand, like [`skills::doc`](crate::skills::doc), for line-numbered errors.
///
/// An entry is a path with a recorded digest (perceived) or a bare path (not
/// yet perceived).
///
/// ```yaml
/// sources:
///   - path: sources/legacy-dump.sql
///     bytes: 18234112
///     sha256: 3f9a…
///   - sources/2026-08-prod.log
/// ```
fn sources(root: &Path) -> (Vec<Source>, Option<String>) {
    let Some(path) = inside(root, SOURCES_FILE) else {
        return (Vec::new(), None);
    };
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return (Vec::new(), None),
        Err(err) => {
            tracing::warn!(%err, path = %path.display(), "the declared sources could not be read");
            return (
                Vec::new(),
                Some(format!("`{SOURCES_FILE}` could not be read: {err}")),
            );
        }
    };

    let mut found: Vec<Source> = Vec::new();
    let mut problem: Option<String> = None;

    for (at, line) in text.lines().enumerate() {
        // `#` always starts a comment, so a path cannot contain one.
        let trimmed = line.split('#').next().unwrap_or("").trim();
        if trimmed.is_empty() || trimmed == "sources:" {
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("- ") {
            let rest = rest.trim();
            let declared = match rest.strip_prefix("path:") {
                Some(value) => value.trim(),
                // The bare form; any other `key:` here is refused, not guessed.
                None if !rest.contains(':') => rest,
                None => {
                    note(
                        &mut problem,
                        at,
                        "an entry starts with a key that is not `path`",
                    );
                    continue;
                }
            };
            match declare(root, declared) {
                Ok(source) => found.push(source),
                Err(what) => note(&mut problem, at, &what),
            }
            continue;
        }

        // A continuation of the entry above it.
        let Some((key, value)) = trimmed.split_once(':') else {
            note(
                &mut problem,
                at,
                "this is neither an entry nor a `key: value`",
            );
            continue;
        };
        let Some(current) = found.last_mut() else {
            note(
                &mut problem,
                at,
                "this describes an entry, but none has started",
            );
            continue;
        };
        let value = value.trim().trim_matches('"');

        match key.trim() {
            "sha256" => current.sha256 = (!value.is_empty()).then(|| value.to_owned()),
            "bytes" => match value.parse::<u64>() {
                Ok(len) => current.bytes = Some(len),
                Err(_) => note(&mut problem, at, "`bytes` is not a number"),
            },
            // Unknown keys (notes, dates) are ignored.
            other => tracing::debug!(key = other, "an unread key in the declared sources"),
        }
    }

    if found.len() > SOURCES_MAX {
        tracing::warn!(
            declared = found.len(),
            kept = SOURCES_MAX,
            "more declared sources than the world block carries"
        );
        found.truncate(SOURCES_MAX);
    }
    (found, problem)
}

/// Records only the *first* thing wrong with `sources.yml`; later ones are
/// usually consequences.
fn note(problem: &mut Option<String>, at: usize, what: &str) {
    if problem.is_none() {
        *problem = Some(format!("`{SOURCES_FILE}` line {}: {what}", at + 1));
    }
}

/// One declared path as a [`Source`], resolved through [`path`] and refused
/// outside the workspace — a model can write `sources.yml`.
fn declare(root: &Path, declared: &str) -> Result<Source, String> {
    let declared = declared.trim().trim_matches('"');
    if declared.is_empty() {
        return Err("this entry declares no path".to_owned());
    }

    let resolved = path::resolve(root, declared).map_err(|err| err.reason().to_owned())?;
    if !resolved.inside {
        return Err(format!(
            "`{declared}` is outside the workspace, so it is not this world's to declare"
        ));
    }

    Ok(Source {
        path: declared.replace('\\', "/"),
        resolved: resolved.path,
        bytes: None,
        sha256: None,
    })
}

// ---------------------------------------------------------------------------
// What reaches the model
// ---------------------------------------------------------------------------

/// The frame and the world's status for the system message.
///
/// Without a world this is the absence paragraph (PLAN 7.17), not `None`: the
/// two layers and the runbook name, nothing else. With a world: file names,
/// declared sources, and only the drift a cheap [`glance`] sees. Never file
/// contents (PLAN 7.1), and never a claim that nothing drifted
/// ([`World::drifted`] is the full measurement).
pub fn block(root: &Path, delegated: bool) -> Option<String> {
    let Some(world) = read(root) else {
        return Some(String::from(if delegated {
            FRAME_ABSENT_DELEGATED
        } else {
            FRAME_ABSENT
        }));
    };
    let mut out = String::from(FRAME);

    // The gate refuses a brief's write but asks about a session's.
    out.push_str("\n\n");
    out.push_str(if delegated {
        FRAME_DELEGATED
    } else {
        FRAME_CABINET
    });

    let held = world
        .held
        .iter()
        .map(|file| format!("`{file}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let _ = write!(out, "\n\n{WORLD_DIR}/ holds: {held}.");

    for leaf in CONSTITUTION
        .iter()
        .filter(|leaf| !world.held.contains(&leaf.file))
    {
        // Named as absent, so the model does not go looking for it.
        let _ = write!(
            out,
            "\nThere is no `{WORLD_DIR}/{}` yet ({}).",
            leaf.file, leaf.what
        );
    }

    if let Some(problem) = &world.problem {
        let _ = write!(out, "\n{problem}. Say so rather than working around it.");
    }
    if world.sources.is_empty() {
        return Some(out);
    }

    let declared = world
        .sources
        .iter()
        .map(|source| format!("`{}`", source.path))
        .collect::<Vec<_>>()
        .join(", ");
    let _ = write!(
        out,
        "\n`{SOURCES_FILE}` declares, and you do not open these: {declared}."
    );

    let moved = world
        .sources
        .iter()
        .filter_map(|source| {
            let state = glance(source).filter(|state| state.is_drift())?;
            Some(format!("`{}` ({})", source.path, state.as_str()))
        })
        .collect::<Vec<_>>();
    if !moved.is_empty() {
        let _ = write!(
            out,
            "\nThese have moved since the world was perceived from them: {}. That is an attention \
             item for whoever owns this world, not something to work around — a delta is perceived \
             deliberately and bounded to what changed.",
            moved.join(", ")
        );
    }

    Some(out)
}

/// Above this, a source is not hashed on the way into a model request (same as
/// the matrix's `READ_ASK_BYTES`).
const GLANCE_MAX_BYTES: u64 = 1024 * 1024;

/// What can be told about a source cheaply: missing, unrecorded or length
/// changed for free, a hash only under [`GLANCE_MAX_BYTES`]. `None` means
/// unknown.
fn glance(source: &Source) -> Option<SourceState> {
    let Ok(meta) = fs::metadata(&source.resolved) else {
        return Some(SourceState::Missing);
    };
    if source.sha256.is_none() {
        return Some(SourceState::Unrecorded);
    }
    if source.bytes.is_some_and(|len| len != meta.len()) {
        return Some(SourceState::Drifted);
    }
    if meta.is_file() && meta.len() <= GLANCE_MAX_BYTES {
        return Some(state_of(source));
    }
    None
}

// ---------------------------------------------------------------------------
// What the gate asks
// ---------------------------------------------------------------------------

/// Whether a workspace-relative path is inside the constitution: first segment
/// only, so `src/world/` is not.
pub fn in_world(relative: &Path) -> bool {
    relative
        .components()
        .find_map(|component| match component {
            Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .is_some_and(|first| first.eq_ignore_ascii_case(WORLD_DIR))
}

/// The declared source `target` is, if it is still in step — a read the gate
/// refuses (PLAN 7.2). A moved source returns `None` so it can be re-perceived.
pub fn perceived_source(root: &Path, target: &Path) -> Option<String> {
    let world = read(root)?;
    let source = world.declaring(target)?;

    (state_of(source) == SourceState::InStep).then(|| source.path.clone())
}

/// Why this brief must not launch while a declared source has drifted
/// (PLAN 7.2). A brief whose `inputs` name a drifted path is the perceive-delta
/// and passes.
pub fn blocking(root: &Path, inputs: &[String]) -> Option<String> {
    let world = read(root)?;
    let moved = world.drifted();
    if moved.is_empty() {
        return None;
    }

    let about_the_delta = moved
        .iter()
        .any(|(source, _)| inputs.iter().any(|input| names(input, &source.path)));
    if about_the_delta {
        return None;
    }

    // How each moved matters: changed, never recorded, or gone.
    let paths: Vec<String> = moved
        .iter()
        .map(|(source, state)| format!("{} — {}", source.path, state.as_str()))
        .collect();
    Some(format!(
        "this world's declared sources have moved since it was perceived from them ({}), and a \
         brief that is not about that delta does not go out on top of it. Route a bounded \
         perceive-delta over those paths first — name them in its `inputs` — or tell the human the \
         world needs amending. Then hand this one out again",
        paths.join("; ")
    ))
}

/// Whether a brief's input names a declared source: a segment-wise suffix
/// match, so relative and absolute spellings both count.
fn names(input: &str, declared: &str) -> bool {
    let input = input.replace('\\', "/");
    let input = input.trim().trim_start_matches("./");

    input.eq_ignore_ascii_case(declared)
        || input
            .strip_suffix(declared)
            .is_some_and(|before| before.ends_with('/'))
}

// ---------------------------------------------------------------------------
// What the panel draws
// ---------------------------------------------------------------------------

/// The world's state in `root`, fully measured — for the panel, not model
/// requests.
pub fn status(root: &Path) -> WorldStatus {
    let world = read(root);
    let held: &[&str] = world.as_ref().map_or(&[], |world| &world.held);

    let files = CONSTITUTION
        .iter()
        .map(|leaf| WorldFile {
            file: format!("{WORLD_DIR}/{}", leaf.file),
            what: leaf.what.to_owned(),
            exists: held.contains(&leaf.file),
        })
        .collect();

    let sources: Vec<WorldSource> = world
        .as_ref()
        .map(|world| {
            world
                .measure()
                .into_iter()
                .map(|(source, state)| WorldSource {
                    path: source.path.clone(),
                    state,
                })
                .collect()
        })
        .unwrap_or_default();

    WorldStatus {
        present: world.is_some(),
        drifted: sources.iter().any(|source| source.state.is_drift()),
        problem: world.and_then(|world| world.problem),
        files,
        sources,
    }
}

/// [`path::resolve`] for reads: a path outside the workspace is simply absent.
fn inside(root: &Path, rel: &str) -> Option<PathBuf> {
    match path::resolve(root, rel) {
        Ok(resolved) if resolved.inside => Some(resolved.path),
        Ok(_) => {
            tracing::debug!(rel, "a world path resolves outside the workspace");
            None
        }
        Err(err) => {
            tracing::debug!(reason = err.reason(), rel, "a world path does not resolve");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    /// A canonical, empty workspace folder.
    fn workspace() -> (TempDir, PathBuf) {
        let dir = TempDir::new().expect("temp dir");
        let root = dunce::canonicalize(dir.path()).expect("canonical");
        (dir, root)
    }

    /// Writes `body` to `rel` inside `root`, creating what it has to.
    fn put(root: &Path, rel: &str, body: &str) -> PathBuf {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
        fs::write(&path, body).expect("write");
        path
    }

    /// A workspace with an essence in it, which is the least a world can be.
    fn founded() -> (TempDir, PathBuf) {
        let (dir, root) = workspace();
        put(&root, "world/essence.md", "# Essence\n\nA thing.\n");
        (dir, root)
    }

    #[test]
    fn a_folder_without_an_essence_is_not_a_world() {
        let (_dir, root) = workspace();
        assert!(read(&root).is_none(), "an empty workspace has no world");

        // A `world/` holding only declared sources is the theatre PLAN 7.2
        // refuses: it would start refusing reads of a dump on the strength of
        // an empty constitution.
        put(
            &root,
            "world/sources.yml",
            "sources:\n  - sources/dump.sql\n",
        );
        assert!(
            read(&root).is_none(),
            "sources alone are not a constitution"
        );
        let absent = block(&root, false).expect("the absence is named");
        assert!(
            absent.contains("no world"),
            "sources alone still have no constitution: {absent}"
        );
        assert!(
            !absent.contains("This workspace has a world"),
            "and must not wear the in-force frame: {absent}"
        );

        put(&root, "world/essence.md", "# Essence\n");
        assert!(read(&root).is_some(), "one file of the constitution is one");
    }

    #[test]
    fn the_absent_frame_stays_small() {
        for frame in [FRAME_ABSENT, FRAME_ABSENT_DELEGATED] {
            assert!(
                frame.chars().count() <= 400,
                "absence is vocabulary, not a runbook: {} chars",
                frame.chars().count()
            );
            assert!(frame.contains("`world/`"), "{frame}");
            assert!(frame.contains("`.aegis/`"), "{frame}");
            assert!(!frame.contains("oracle.md"), "no file checklist: {frame}");
        }
        assert!(FRAME_ABSENT.contains("world.draft"), "{FRAME_ABSENT}");
        assert!(
            !FRAME_ABSENT_DELEGATED.contains("world.draft"),
            "a brief is not pointed at the runbook it may not finish: {FRAME_ABSENT_DELEGATED}"
        );
    }

    #[test]
    fn the_block_frames_the_session_and_never_carries_the_essence() {
        let (_dir, root) = founded();

        for delegated in [true, false] {
            let block = block(&root, delegated).expect("a world in force");

            assert!(block.contains("écart"), "{block}");
            assert!(
                !block.contains("This workspace has no world"),
                "a founded world is not the absence paragraph: {block}"
            );
            assert!(block.contains("`essence.md`"), "the file is named: {block}");
            assert!(
                !block.contains("A thing."),
                "the essence itself stays on disk: {block}"
            );
            // What is missing is named too, so the model says so rather than
            // hunting for a file nobody has written.
            assert!(block.contains("world/oracle.md"), "{block}");
        }

        // The write rule is the half that differs, because the gate differs: a
        // brief's write is refused, a session's is put to the operator. Telling
        // a session it may not write is a prompt refusing what the gate would
        // have asked about, which is the harder failure to see — there is no
        // audit line for a call nobody made.
        let brief = block(&root, true).expect("a world");
        let session = block(&root, false).expect("a world");

        assert!(brief.contains("You do not write `world/`"), "{brief}");
        assert!(brief.contains("needs_you"), "{brief}");
        assert!(!session.contains("You do not write `world/`"), "{session}");
        assert!(session.contains("world.draft"), "{session}");
    }

    #[test]
    fn a_declared_source_is_read_in_both_spellings_and_never_outside_the_folder() {
        let (_dir, root) = founded();
        put(&root, "sources/dump.sql", "select 1;\n");
        put(&root, "sources/prod.log", "one\n");
        put(
            &root,
            "world/sources.yml",
            "# what this world was perceived from\n\
             sources:\n\
             \x20 - path: sources/dump.sql\n\
             \x20   bytes: 10\n\
             \x20   sha256: deadbeef   # not the real one\n\
             \x20 - sources/prod.log\n\
             \x20 - path: ../escape.txt\n",
        );

        let world = read(&root).expect("a world");
        let declared: Vec<&str> = world.sources.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(
            declared,
            ["sources/dump.sql", "sources/prod.log"],
            "the escape is refused, the two spellings are both read"
        );
        assert!(
            world
                .problem
                .as_deref()
                .is_some_and(|p| p.contains("outside")),
            "and the refusal is reported: {:?}",
            world.problem
        );

        assert_eq!(world.sources[0].bytes, Some(10));
        assert_eq!(world.sources[0].sha256.as_deref(), Some("deadbeef"));
        assert!(
            world.sources[1].sha256.is_none(),
            "a bare path records nothing"
        );
    }

    #[test]
    fn a_source_is_in_step_only_when_its_bytes_still_hash_to_what_was_recorded() {
        let (_dir, root) = founded();
        let dump = put(&root, "sources/dump.sql", "select 1;\n");
        let recorded = hash(&dump).expect("hash");
        let bytes = fs::metadata(&dump).expect("metadata").len();

        put(
            &root,
            "world/sources.yml",
            &format!(
                "sources:\n  - path: sources/dump.sql\n    bytes: {bytes}\n    sha256: {recorded}\n"
            ),
        );
        let world = read(&root).expect("a world");
        assert_eq!(world.measure()[0].1, SourceState::InStep);
        assert!(world.drifted().is_empty());

        // Appended to: the length alone says it moved, and nothing has to be
        // hashed to know it.
        fs::write(&dump, "select 1;\nselect 2;\n").expect("write");
        let world = read(&root).expect("a world");
        assert_eq!(world.measure()[0].1, SourceState::Drifted);
        assert_eq!(glance(&world.sources[0]), Some(SourceState::Drifted));

        // Rewritten to the same length: only the digest catches this one. A
        // glance still does, because this dump is small enough that reading it
        // is not the round-trip the module is written around.
        fs::write(&dump, "select 2;\n").expect("write");
        let world = read(&root).expect("a world");
        assert_eq!(glance(&world.sources[0]), Some(SourceState::Drifted));
        assert_eq!(world.measure()[0].1, SourceState::Drifted);

        fs::remove_file(&dump).expect("remove");
        let world = read(&root).expect("a world");
        assert_eq!(world.measure()[0].1, SourceState::Missing);
    }

    /// The one thing a glance will not do is read a dump to answer a system
    /// message. Above [`GLANCE_MAX_BYTES`] the block says nothing rather than
    /// paying for the round-trip the world exists to have paid once — and the
    /// full measurement, which runs where drift changes a decision, still finds
    /// it.
    #[test]
    fn a_large_source_is_not_hashed_on_the_way_into_a_prompt() {
        let (_dir, root) = founded();
        let big = "x".repeat((GLANCE_MAX_BYTES + 1) as usize);
        let dump = put(&root, "sources/dump.sql", &big);
        let recorded = hash(&dump).expect("hash");
        put(
            &root,
            "world/sources.yml",
            &format!(
                "sources:\n  - path: sources/dump.sql\n    bytes: {}\n    sha256: {recorded}\n",
                big.len()
            ),
        );

        // Rewritten to exactly the same length, so nothing but the digest can
        // tell — and the digest is what a glance will not pay for here.
        fs::write(&dump, "y".repeat(big.len())).expect("write");
        let world = read(&root).expect("a world");
        assert_eq!(glance(&world.sources[0]), None);
        assert!(
            !block(&root, false).expect("a block").contains("have moved"),
            "the block reports what it saw, and never claims there was nothing"
        );
        assert_eq!(world.measure()[0].1, SourceState::Drifted);
        assert!(blocking(&root, &[".aegis/briefs/one.md".to_owned()]).is_some());
    }

    #[test]
    fn a_source_nothing_was_ever_perceived_from_is_drift() {
        let (_dir, root) = founded();
        put(&root, "sources/dump.sql", "select 1;\n");
        put(
            &root,
            "world/sources.yml",
            "sources:\n  - sources/dump.sql\n",
        );

        let world = read(&root).expect("a world");
        assert_eq!(world.measure()[0].1, SourceState::Unrecorded);
        assert!(
            SourceState::Unrecorded.is_drift(),
            "and it is an attention item"
        );
        assert!(
            block(&root, false).expect("a block").contains("have moved"),
            "which the frame says out loud"
        );
    }

    #[test]
    fn a_perceived_source_is_refused_and_a_moved_one_is_not() {
        let (_dir, root) = founded();
        let dump = put(&root, "sources/dump.sql", "select 1;\n");
        let recorded = hash(&dump).expect("hash");
        put(
            &root,
            "world/sources.yml",
            &format!("sources:\n  - path: sources/dump.sql\n    sha256: {recorded}\n"),
        );

        assert_eq!(
            perceived_source(&root, &dump).as_deref(),
            Some("sources/dump.sql"),
            "already perceived: reading it again is the round-trip the world paid for"
        );
        assert!(
            perceived_source(&root, &root.join("world/essence.md")).is_none(),
            "the constitution itself is not a source"
        );

        // The delta is the one legitimate re-perception, so it reads like any
        // other file in the workspace.
        fs::write(&dump, "select 1;\nselect 2;\n").expect("write");
        assert!(
            perceived_source(&root, &dump).is_none(),
            "a moved source reads"
        );
    }

    #[test]
    fn only_the_first_segment_of_a_path_is_the_constitution() {
        assert!(in_world(Path::new("world/essence.md")));
        assert!(
            in_world(Path::new("World/sources.yml")),
            "and case does not matter"
        );
        assert!(in_world(Path::new("world")));
        assert!(
            !in_world(Path::new("src/world/mod.rs")),
            "a module called world is not a constitution"
        );
        assert!(!in_world(Path::new(".aegis/briefs/one.md")));
    }

    #[test]
    fn drift_stops_every_brief_except_the_one_about_the_delta() {
        let (_dir, root) = founded();
        put(&root, "sources/dump.sql", "select 1;\n");
        put(
            &root,
            "world/sources.yml",
            "sources:\n  - sources/dump.sql\n",
        );

        let refusal = blocking(&root, &[".aegis/briefs/one.md".to_owned()]).expect("blocked");
        assert!(refusal.contains("sources/dump.sql"), "{refusal}");
        assert!(refusal.contains("perceive-delta"), "{refusal}");

        for input in [
            "sources/dump.sql",
            "./sources/dump.sql",
            "sources\\dump.sql",
        ] {
            assert!(
                blocking(&root, &[input.to_owned()]).is_none(),
                "`{input}` names the delta"
            );
        }
        assert!(
            blocking(&root, &["other/dump.sql".to_owned()]).is_some(),
            "a different file that ends the same way is not the delta"
        );
    }

    #[test]
    fn a_workspace_with_no_world_blocks_nothing_and_names_the_layers() {
        let (_dir, root) = workspace();

        assert!(blocking(&root, &[]).is_none());
        let absent = block(&root, false).expect("the absence is named");
        assert!(absent.contains("`world/`"), "{absent}");
        assert!(absent.contains("`.aegis/`"), "{absent}");
        assert!(absent.contains("world.draft"), "{absent}");
        assert!(
            !absent.contains("écart"),
            "the in-force frame stays off: {absent}"
        );
        let brief = block(&root, true).expect("a brief is told too");
        assert!(
            !brief.contains("world.draft"),
            "a brief is not pointed at the runbook: {brief}"
        );
        assert!(perceived_source(&root, &root.join("anything")).is_none());

        let found = status(&root);
        assert!(!found.present);
        assert!(!found.drifted);
        assert!(found.sources.is_empty());
        assert_eq!(
            found.files.len(),
            CONSTITUTION.len(),
            "still named, so the panel can say what a world is"
        );
        assert!(found.files.iter().all(|file| !file.exists));
    }

    #[test]
    fn the_panel_measures_the_folder_rather_than_remembering_it() {
        let (_dir, root) = founded();
        let dump = put(&root, "sources/dump.sql", "select 1;\n");
        let recorded = hash(&dump).expect("hash");
        put(
            &root,
            "world/sources.yml",
            &format!("sources:\n  - path: sources/dump.sql\n    sha256: {recorded}\n"),
        );

        let found = status(&root);
        assert!(found.present);
        assert!(!found.drifted);
        assert_eq!(found.sources[0].state, SourceState::InStep);
        assert!(found
            .files
            .iter()
            .any(|f| f.file == "world/essence.md" && f.exists));
        assert!(found
            .files
            .iter()
            .any(|f| f.file == "world/oracle.md" && !f.exists));

        fs::write(&dump, "select 2;\nselect 3;\n").expect("write");
        let found = status(&root);
        assert!(found.drifted);
        assert_eq!(found.sources[0].state, SourceState::Drifted);
    }
}
