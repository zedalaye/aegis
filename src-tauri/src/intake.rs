//! A file dropped onto the project becomes a brief (PLAN 7.15).
//!
//! The one write the explorer is allowed, and it is the operator's rather than
//! the agent's — the same class as *Set up shared files*, and as picking the
//! folder in the first place. So it does not go through the approval dialog. It
//! does leave an audit line: a brief that arrived from outside with no record is
//! intake nobody can see afterwards.
//!
//! What it will and will not do is narrow on purpose.
//!
//! * **Only into `.aegis/briefs/`.** A brief is work going *in*. There is no
//!   destination argument anywhere in this module, so a drop cannot land in
//!   `.aegis/artefacts/` — work coming *out*, written under the gate — or in
//!   `world/`, the constitution. The window refuses those targets before it
//!   asks; this module could not honour them if it did.
//! * **Only paths the OS handed this process.** A drop is recorded by the
//!   window-event handler in `lib.rs`, under an id, and the command names that
//!   id. The WebView never supplies a source path, so this is not a way to copy
//!   an arbitrary file off the machine into a workspace.
//! * **Copy, never move, never overwrite.** The original stays where the
//!   operator keeps it. A name that is taken keeps both, the promise scaffold
//!   already makes.
//! * **The file is the input.** No generated markdown around it, no runbook
//!   inferred from its extension, no turn started. The next `fs_list` of
//!   `.aegis/briefs/` is how a runbook finds it (PLAN 7.6: a file in
//!   `.aegis/briefs/` is a valid input).
//! * **A drop does not lay down the convention.** A workspace without
//!   `.aegis/briefs/` refuses; creating it is a separate press.

use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::json;
use ts_rs::TS;

use crate::audit::{AuditDecision, AuditEntry, AuditLog, AuditRecord, Outcome};
use crate::error::{AppError, AppResult, ErrorCode};
use crate::policy::path;
use crate::workspace::BRIEFS_DIR;

/// How long a drop is held for the window to name it.
///
/// The window asks within milliseconds of the event. The allowance is for the
/// other path: a drop onto a workspace with no `.aegis/briefs/` yet, where the
/// operator presses *Set up shared files* and then adds what they dropped.
pub const DROP_TTL: Duration = Duration::from_secs(120);

/// The name an arrival is recorded under in the audit log.
///
/// Not a tool — no model can call it — but the log's `tool` column is what a
/// person scans, and this is what they are looking for when intake appears
/// without a session behind it.
pub const BRIEF_IMPORT: &str = "brief_import";

/// Longest name a brief is given, in characters.
const NAME_MAX_CHARS: usize = 120;

/// Names Windows will not create, whatever comes after the dot.
///
/// Refused on every platform: a workspace is a folder that gets committed and
/// cloned, and a brief called `aux.txt` made on a Mac is a checkout that fails
/// on somebody's PC.
const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// `workspace:dropped` — the OS put files on the window.
///
/// Names and a position, never bytes. The position is in physical pixels from
/// the top left of the webview, as the OS reported it; the window divides by
/// its own scale to find which row the drop landed on.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct WorkspaceDropped {
    /// What `workspace_import_brief` is called with.
    pub drop_id: String,
    /// The dropped files' own names, for the line that reports them.
    pub names: Vec<String>,
    /// Physical pixels from the left of the webview.
    pub x: f64,
    /// Physical pixels from the top of the webview.
    pub y: f64,
}

/// One file that arrived.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct BriefArrival {
    /// The name it had where the operator keeps it.
    pub from: String,
    /// Where it is now, relative to the workspace root.
    pub path: String,
    /// How much was copied.
    #[ts(type = "number")]
    pub bytes: u64,
}

/// One file that did not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct BriefRefusal {
    /// The name it had where the operator keeps it.
    pub name: String,
    /// Why it was not copied, in words somebody can act on.
    pub reason: String,
}

/// What one drop did.
///
/// Two lists, for the reason scaffold's report has two: the operator dropped
/// several things, and the answer to "did they all arrive" has to name the ones
/// that did not.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct ImportReport {
    /// Files copied into `.aegis/briefs/`, in the order they were dropped.
    pub arrived: Vec<BriefArrival>,
    /// Files left where they were, and why.
    pub refused: Vec<BriefRefusal>,
}

// ---------------------------------------------------------------------------
// The drop the OS handed over
// ---------------------------------------------------------------------------

/// The last drop, held until the window names it.
///
/// One slot rather than a map. A person drops one armful at a time, and a
/// second drop replacing an unclaimed first is exactly what should happen to
/// something nobody acted on.
#[derive(Debug, Default)]
pub struct Drops {
    held: Mutex<Option<Held>>,
}

#[derive(Debug)]
struct Held {
    id: String,
    paths: Vec<PathBuf>,
    at: Instant,
}

impl Drops {
    /// Nothing held.
    pub fn new() -> Self {
        Self::default()
    }

    /// Holds what the OS dropped on the window, and returns its id.
    pub fn record(&self, paths: Vec<PathBuf>) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        *self.lock() = Some(Held {
            id: id.clone(),
            paths,
            at: Instant::now(),
        });
        id
    }

    /// Whether `id` is the drop being held, and still fresh.
    pub fn holds(&self, id: &str) -> bool {
        self.lock()
            .as_ref()
            .is_some_and(|held| held.id == id && held.at.elapsed() <= DROP_TTL)
    }

    /// Hands the drop's paths back, once.
    ///
    /// `None` for an id that is not the one held, or one held too long. A
    /// mismatched id leaves the held drop alone: a stale window naming an old
    /// drop must not throw away the one somebody just made.
    pub fn take(&self, id: &str) -> Option<Vec<PathBuf>> {
        self.take_at(id, Instant::now())
    }

    fn take_at(&self, id: &str, now: Instant) -> Option<Vec<PathBuf>> {
        let mut held = self.lock();
        if !held.as_ref().is_some_and(|held| held.id == id) {
            return None;
        }
        held.take()
            .filter(|held| now.saturating_duration_since(held.at) <= DROP_TTL)
            .map(|held| held.paths)
    }

    /// Poisoning is recovered: the slot holds paths, not an invariant.
    fn lock(&self) -> MutexGuard<'_, Option<Held>> {
        self.held
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

/// The workspace's `.aegis/briefs/`, or why a drop cannot land there.
///
/// Resolved rather than joined, so a `briefs` that has become a link to another
/// disk refuses instead of receiving somebody's files. Never created: a drop is
/// not consent to lay down the convention.
pub fn briefs_dir(root: &Path) -> AppResult<PathBuf> {
    let resolved = path::resolve(root, BRIEFS_DIR).map_err(|err| AppError::BriefImport {
        reason: err.reason().to_owned(),
    })?;

    if !resolved.inside || resolved.escaped() {
        return Err(AppError::BriefImport {
            reason: "`.aegis/briefs` points outside this workspace".to_owned(),
        });
    }
    if !resolved.path.is_dir() {
        return Err(AppError::BriefImport {
            reason: "this workspace has no `.aegis/briefs/` yet. Set up shared files lays it \
                     down; a drop does not"
                .to_owned(),
        });
    }
    Ok(resolved.path)
}

/// Copies each dropped file into `briefs`, and audits each arrival.
///
/// `briefs` is what [`briefs_dir`] returned. Returns the report and the lines
/// written, so the command can announce them on `audit:appended` the way a
/// tool call's line is announced.
pub fn import(
    briefs: &Path,
    sources: &[PathBuf],
    audit: &AuditLog,
    project_id: &str,
    drop_id: &str,
) -> (ImportReport, Vec<AuditEntry>) {
    let mut report = ImportReport::default();
    let mut lines = Vec::new();

    for (index, source) in sources.iter().enumerate() {
        let shown = source.file_name().map_or_else(
            || source.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let refuse = |reason: &str| BriefRefusal {
            name: shown.clone(),
            reason: reason.to_owned(),
        };

        match fs::metadata(source) {
            Err(_) => {
                report.refused.push(refuse("it is not there any more"));
                continue;
            }
            Ok(meta) if meta.is_dir() => {
                report
                    .refused
                    .push(refuse("a folder is not a brief; drop the files inside it"));
                continue;
            }
            Ok(meta) if !meta.is_file() => {
                report.refused.push(refuse("it is not an ordinary file"));
                continue;
            }
            Ok(_) => {}
        }

        let started = Instant::now();
        let name = brief_name(&shown);
        let copied = copy_in(source, briefs, &name);
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

        let (landed, outcome, bytes) = match &copied {
            Ok((landed, bytes)) => (landed.as_str(), Outcome::Ok, *bytes),
            Err(_) => (name.as_str(), Outcome::Error, 0),
        };
        let rel = format!("{BRIEFS_DIR}/{landed}");
        let args = json!({
            "project_id": project_id,
            "from": source.display().to_string(),
            "path": rel,
        });
        let call_id = format!("{drop_id}:{index}");

        lines.push(audit.append(&AuditRecord {
            session_id: "",
            agent_id: "",
            turn_id: "",
            call_id: &call_id,
            tool: BRIEF_IMPORT,
            skill: "",
            handoff: "",
            routine: "",
            decision: AuditDecision::Operator,
            policy_reason: "dropped onto the project in the window: copied into .aegis/briefs/, \
                            not moved, and no approval asked",
            args: &args,
            outcome,
            duration_ms,
            bytes_in: bytes,
            bytes_out: 0,
            error_code: copied.is_err().then_some(ErrorCode::ToolFailed),
            artifact: None,
        }));

        match copied {
            Ok(_) => report.arrived.push(BriefArrival {
                from: shown,
                path: rel,
                bytes,
            }),
            Err(err) => {
                tracing::warn!(%err, source = %source.display(), "a dropped file could not be copied");
                report.refused.push(refuse(match err.kind() {
                    io::ErrorKind::PermissionDenied => {
                        "it could not be read, or briefs is not writable"
                    }
                    io::ErrorKind::AlreadyExists => "every name like it is already taken",
                    _ => "it could not be copied",
                }));
            }
        }
    }

    tracing::info!(
        arrived = report.arrived.len(),
        refused = report.refused.len(),
        "a drop was imported as briefs"
    );
    (report, lines)
}

/// Copies one file into `dir` under a name nothing else has.
///
/// A copy that fails halfway removes what it wrote: a truncated brief is worse
/// than no brief, because a runbook would read it.
fn copy_in(source: &Path, dir: &Path, name: &str) -> io::Result<(String, u64)> {
    let mut from = fs::File::open(source)?;
    let (mut to, landed) = create_unique(dir, name)?;

    match io::copy(&mut from, &mut to) {
        Ok(bytes) => Ok((landed, bytes)),
        Err(err) => {
            drop(to);
            if let Err(cleanup) = fs::remove_file(dir.join(&landed)) {
                tracing::warn!(%cleanup, landed, "a partial brief could not be removed");
            }
            Err(err)
        }
    }
}

/// Creates `name` in `dir`, or `name (2)`, `name (3)`, … when it is taken.
///
/// `create_new` is the whole guarantee: the check and the creation are one
/// system call, so nothing that appears in between is overwritten — and a link
/// planted under a candidate name counts as taken rather than being followed.
fn create_unique(dir: &Path, name: &str) -> io::Result<(fs::File, String)> {
    let (stem, ext) = split_extension(name);

    for n in 1..=999 {
        let candidate = if n == 1 {
            name.to_owned()
        } else {
            format!("{stem} ({n}){ext}")
        };
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join(&candidate))
        {
            Ok(file) => return Ok((file, candidate)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::from(io::ErrorKind::AlreadyExists))
}

/// A dropped file's name, made safe to create on any platform the folder may
/// be cloned to.
///
/// Separators and the characters Windows refuses become `_`; leading dots go,
/// so a drop never makes a hidden file or a `..`; trailing dots and spaces go,
/// because Windows strips them and two names would collide; a reserved device
/// name is prefixed. What is left is the name the operator recognises, not a
/// generated one.
pub fn brief_name(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();

    let trimmed = cleaned
        .trim()
        .trim_start_matches('.')
        .trim_end_matches(['.', ' '])
        .trim();
    let mut name = if trimmed.is_empty() {
        "dropped".to_owned()
    } else {
        trimmed.to_owned()
    };

    let device = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_uppercase();
    if RESERVED.contains(&device.as_str()) {
        name.insert(0, '_');
    }

    if name.chars().count() > NAME_MAX_CHARS {
        let (stem, ext) = split_extension(&name);
        let keep = NAME_MAX_CHARS.saturating_sub(ext.chars().count()).max(1);
        name = format!("{}{ext}", stem.chars().take(keep).collect::<String>());
    }
    name
}

/// `("report", ".csv")`, or the whole name and nothing when there is no short
/// extension worth keeping.
fn split_extension(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(at) if at > 0 && name.len() - at <= 16 => name.split_at(at),
        _ => (name, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    struct Setup {
        _dir: TempDir,
        root: PathBuf,
        outside: PathBuf,
        audit: AuditLog,
    }

    fn setup(with_briefs: bool) -> Setup {
        let dir = TempDir::new().expect("temp dir");
        let root = dir.path().join("work");
        let outside = dir.path().join("desktop");
        fs::create_dir_all(&root).expect("workspace");
        fs::create_dir_all(&outside).expect("somewhere else");
        if with_briefs {
            fs::create_dir_all(root.join(BRIEFS_DIR)).expect("briefs");
        }
        let audit = AuditLog::new(&dir.path().join("data"));
        Setup {
            root: dunce::canonicalize(&root).expect("canonical"),
            outside,
            audit,
            _dir: dir,
        }
    }

    #[test]
    fn a_dropped_file_is_copied_into_briefs_and_left_where_it_was() {
        let s = setup(true);
        let source = s.outside.join("q3 export.csv");
        fs::write(&source, "a,b\n1,2\n").expect("write");

        let briefs = briefs_dir(&s.root).expect("briefs");
        let (report, lines) = import(&briefs, std::slice::from_ref(&source), &s.audit, "p1", "d1");

        assert_eq!(
            report.arrived,
            [BriefArrival {
                from: "q3 export.csv".to_owned(),
                path: ".aegis/briefs/q3 export.csv".to_owned(),
                bytes: 8,
            }]
        );
        assert!(report.refused.is_empty());
        assert_eq!(
            fs::read_to_string(s.root.join(".aegis/briefs/q3 export.csv")).expect("read"),
            "a,b\n1,2\n",
            "the file is the input, not a wrapper around it"
        );
        assert!(source.is_file(), "copied, not moved");
        assert_eq!(
            fs::read_dir(&briefs).expect("list").count(),
            1,
            "nothing generated beside it"
        );
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn an_arrival_is_audited_as_the_operators_act_with_no_session() {
        let s = setup(true);
        let source = s.outside.join("mail.eml");
        fs::write(&source, "From: someone").expect("write");

        let briefs = briefs_dir(&s.root).expect("briefs");
        import(&briefs, std::slice::from_ref(&source), &s.audit, "p1", "d1");

        let tail = s.audit.tail(10, None).expect("tail");
        assert_eq!(tail.len(), 1);
        let line = &tail[0];
        assert_eq!(line.tool, BRIEF_IMPORT);
        assert_eq!(line.decision, AuditDecision::Operator);
        assert_eq!(line.session_id, "", "no session wrote it");
        assert_eq!(line.outcome, Outcome::Ok);
        assert_eq!(line.bytes_in, 13);
        assert!(
            line.args_redacted.contains(".aegis/briefs/mail.eml"),
            "{}",
            line.args_redacted
        );
        assert!(
            line.args_redacted
                .contains(&*source.display().to_string().replace('\\', "\\\\")),
            "where it came from is kept whole: {}",
            line.args_redacted
        );
        assert!(
            !line.args_redacted.contains("From: someone"),
            "and never what it says"
        );
    }

    #[test]
    fn a_name_that_is_taken_keeps_both() {
        let s = setup(true);
        fs::write(s.root.join(".aegis/briefs/notes.md"), "ours").expect("write");
        let source = s.outside.join("notes.md");
        fs::write(&source, "theirs").expect("write");

        let briefs = briefs_dir(&s.root).expect("briefs");
        let (first, _) = import(&briefs, std::slice::from_ref(&source), &s.audit, "p1", "d1");
        let (second, _) = import(&briefs, &[source], &s.audit, "p1", "d2");

        assert_eq!(first.arrived[0].path, ".aegis/briefs/notes (2).md");
        assert_eq!(second.arrived[0].path, ".aegis/briefs/notes (3).md");
        assert_eq!(
            fs::read_to_string(s.root.join(".aegis/briefs/notes.md")).expect("read"),
            "ours",
            "nothing is overwritten"
        );
    }

    #[test]
    fn a_folder_is_refused_and_the_files_beside_it_still_arrive() {
        let s = setup(true);
        let folder = s.outside.join("inbox");
        fs::create_dir_all(&folder).expect("dir");
        let file = s.outside.join("brief.md");
        fs::write(&file, "goal").expect("write");
        let gone = s.outside.join("gone.txt");

        let briefs = briefs_dir(&s.root).expect("briefs");
        let (report, lines) = import(&briefs, &[folder, file, gone], &s.audit, "p1", "d1");

        assert_eq!(report.arrived.len(), 1);
        let refused: Vec<&str> = report.refused.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(refused, ["inbox", "gone.txt"]);
        assert_eq!(lines.len(), 1, "only what arrived, or failed to, is a line");
    }

    #[test]
    fn a_workspace_without_briefs_refuses_and_creates_nothing() {
        let s = setup(false);

        assert!(matches!(
            briefs_dir(&s.root),
            Err(AppError::BriefImport { .. })
        ));
        assert!(
            !s.root.join(".aegis").exists(),
            "a drop is not consent to lay down the convention"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_briefs_folder_that_points_elsewhere_refuses() {
        let s = setup(false);
        fs::create_dir_all(s.root.join(".aegis")).expect("cabinet");
        std::os::unix::fs::symlink(&s.outside, s.root.join(BRIEFS_DIR)).expect("symlink");

        assert!(matches!(
            briefs_dir(&s.root),
            Err(AppError::BriefImport { .. })
        ));
    }

    #[test]
    fn names_are_made_safe_and_stay_recognisable() {
        assert_eq!(brief_name("q3 export.csv"), "q3 export.csv");
        assert_eq!(brief_name("café.md"), "café.md");
        assert_eq!(brief_name("a/b\\c:d.txt"), "a_b_c_d.txt");
        assert_eq!(brief_name(".env"), "env");
        assert_eq!(brief_name(".."), "dropped");
        assert_eq!(brief_name("report. "), "report");
        assert_eq!(brief_name(""), "dropped");
        assert_eq!(brief_name("CON.txt"), "_CON.txt");
        assert_eq!(brief_name("con"), "_con");
        assert_eq!(brief_name("line\nbreak.md"), "line_break.md");

        let long = format!("{}.pdf", "x".repeat(300));
        let capped = brief_name(&long);
        assert_eq!(capped.chars().count(), NAME_MAX_CHARS);
        assert!(capped.ends_with(".pdf"), "{capped}");
    }

    #[test]
    fn a_drop_is_handed_back_once_and_only_by_its_id() {
        let drops = Drops::new();
        let id = drops.record(vec![PathBuf::from("a.txt")]);

        assert!(drops.holds(&id));
        assert_eq!(drops.take("some-other-id"), None);
        assert!(drops.holds(&id), "a wrong id does not discard the real one");
        assert_eq!(drops.take(&id), Some(vec![PathBuf::from("a.txt")]));
        assert_eq!(drops.take(&id), None, "once");
    }

    #[test]
    fn a_new_drop_replaces_one_nobody_claimed() {
        let drops = Drops::new();
        let first = drops.record(vec![PathBuf::from("a.txt")]);
        let second = drops.record(vec![PathBuf::from("b.txt")]);

        assert_eq!(drops.take(&first), None);
        assert_eq!(drops.take(&second), Some(vec![PathBuf::from("b.txt")]));
    }

    #[test]
    fn a_drop_held_too_long_is_gone() {
        let drops = Drops::new();
        let id = drops.record(vec![PathBuf::from("a.txt")]);

        let later = Instant::now() + DROP_TTL + Duration::from_secs(1);
        assert_eq!(drops.take_at(&id, later), None);
        assert!(!drops.holds(&id), "and it is not kept around afterwards");
    }
}
