//! On-disk persistence.
//!
//! One JSON document, `projects.json`, under the OS application-data
//! directory. It is small, human-readable and hand-editable on purpose: a
//! project is a name plus a workspace path, and a user who has to recover
//! from a bad state should be able to open the file and see why.
//!
//! Three properties matter more than the format:
//!
//! * **Writes are atomic.** The document is written to a sibling temporary
//!   file, flushed, then renamed over the target. A crash or a power cut
//!   leaves either the old document or the new one, never a half-written one.
//! * **A damaged document never blocks the app.** Unparseable content is
//!   moved aside with a timestamped name and the app starts with an empty
//!   list, because a tray app that refuses to boot has no way to tell anyone
//!   why.
//! * **`workspace_exists` is never persisted.** Whether a folder is still
//!   there is a fact about the disk right now, so it is measured on every
//!   read. A stored copy would be wrong the moment a drive is unplugged.
//!
//! Phase 5 adds sessions here, beside the projects, behind the same atomic
//! write.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{AppError, AppResult};

/// Name of the document under the application-data directory.
const PROJECTS_FILE: &str = "projects.json";

/// Schema version of [`ProjectsFile`].
///
/// A document carrying anything else is treated exactly like a damaged one:
/// quarantined, not guessed at. Bumping this is how a future migration
/// announces itself.
const SCHEMA_VERSION: u32 = 1;

/// Rename attempts before a failed save gives up.
///
/// The replace step is a single `MoveFileEx` on Windows, which an antivirus or
/// an indexer holding the old file open can make fail for a few milliseconds
/// (see the README's Windows notes). Retrying briefly turns a transient
/// scanner collision back into a successful save.
const RENAME_ATTEMPTS: u32 = 3;
const RENAME_BACKOFF: Duration = Duration::from_millis(20);

// ---------------------------------------------------------------------------
// IPC payloads (PLAN 2.1, "Projects")
// ---------------------------------------------------------------------------

/// A project as the UI sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Project {
    /// UUID v4, stable for the life of the project.
    pub id: String,
    /// Display name. Defaults to the workspace folder name.
    pub name: String,
    /// Canonicalized absolute path, free of Windows verbatim prefixes.
    pub workspace_path: String,
    /// RFC3339, UTC.
    pub created_at: String,
    /// RFC3339, UTC. `None` until the project has been opened once.
    pub last_opened_at: Option<String>,
    /// Whether the workspace folder is present *right now*. Never stored.
    pub workspace_exists: bool,
}

/// Lifecycle of a session (PLAN 2.1, "Sessions and turns").
///
/// Declared with [`SessionSummary`] so [`ProjectDetail`] has its documented
/// shape from the first command that returns it; Phase 5 is what starts
/// producing values other than the empty list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Idle,
    Running,
    AwaitingApproval,
    Error,
}

/// One row of a project's session list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionSummary {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub message_count: u32,
    pub state: SessionState,
}

/// What opening a project yields: the project plus its sessions, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDetail {
    pub project: Project,
    /// Always empty until Phase 5 introduces sessions.
    pub sessions: Vec<SessionSummary>,
}

// ---------------------------------------------------------------------------
// On-disk shapes
// ---------------------------------------------------------------------------

/// The document itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProjectsFile {
    version: u32,
    projects: Vec<StoredProject>,
}

/// A project record as persisted.
///
/// Deliberately not [`Project`]: `workspace_exists` is derived on read, and
/// keeping the two types apart makes it impossible to persist it by accident.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredProject {
    id: String,
    name: String,
    workspace_path: String,
    created_at: String,
    #[serde(default)]
    last_opened_at: Option<String>,
}

impl StoredProject {
    /// Adds the live filesystem check the UI needs.
    fn to_project(&self) -> Project {
        Project {
            id: self.id.clone(),
            name: self.name.clone(),
            workspace_path: self.workspace_path.clone(),
            created_at: self.created_at.clone(),
            last_opened_at: self.last_opened_at.clone(),
            workspace_exists: Path::new(&self.workspace_path).is_dir(),
        }
    }
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// The project store: the in-memory list plus the document backing it.
///
/// The whole list is held under one mutex and written out on every mutation.
/// That is the right trade at this size — a handful of records, changed only
/// by a human clicking — and it makes "what is on disk" always equal to "what
/// is in memory" after a command returns, with no flush to forget.
#[derive(Debug)]
pub struct Store {
    path: PathBuf,
    projects: Mutex<Vec<StoredProject>>,
}

impl Store {
    /// Loads the store from `data_dir`, which is created if missing.
    ///
    /// Never fails. A store that cannot be read starts empty and says so in
    /// the log; the failure resurfaces honestly on the first save, where there
    /// is a user waiting for an answer and an error can be shown.
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(PROJECTS_FILE);

        if let Err(err) = fs::create_dir_all(data_dir) {
            tracing::error!(%err, dir = %data_dir.display(), "could not create the data directory");
        }

        let projects = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<ProjectsFile>(strip_bom(&bytes)) {
                Ok(file) if file.version == SCHEMA_VERSION => {
                    tracing::info!(count = file.projects.len(), "project store loaded");
                    file.projects
                }
                Ok(file) => {
                    tracing::error!(
                        found = file.version,
                        expected = SCHEMA_VERSION,
                        "unknown project store version"
                    );
                    quarantine(&path);
                    Vec::new()
                }
                Err(err) => {
                    tracing::error!(%err, "project store is not readable JSON");
                    quarantine(&path);
                    Vec::new()
                }
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                tracing::info!("no project store yet; starting empty");
                Vec::new()
            }
            Err(err) => {
                tracing::error!(%err, "could not read the project store");
                Vec::new()
            }
        };

        Self {
            path,
            projects: Mutex::new(projects),
        }
    }

    /// Locks the list.
    ///
    /// A poisoned mutex means some other command panicked mid-mutation. The
    /// data behind it is a plain `Vec` that is only ever replaced wholesale,
    /// so it cannot be torn: recovering the inner value is strictly better
    /// than propagating a panic through every later command.
    fn projects(&self) -> MutexGuard<'_, Vec<StoredProject>> {
        self.projects
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Every project, most recently opened first.
    ///
    /// Timestamps are fixed-width UTC RFC3339, so the reverse lexicographic
    /// order below *is* reverse chronological order. A project never opened
    /// falls back to its creation time, which keeps a fresh project at the top
    /// where the user just put it.
    pub fn list(&self) -> Vec<Project> {
        let projects = self.projects();

        let mut out: Vec<Project> = projects.iter().map(StoredProject::to_project).collect();
        out.sort_by(|a, b| {
            let key = |p: &Project| {
                p.last_opened_at
                    .clone()
                    .unwrap_or_else(|| p.created_at.clone())
            };
            key(b).cmp(&key(a)).then_with(|| a.name.cmp(&b.name))
        });
        out
    }

    /// Registers a workspace folder as a project.
    ///
    /// `workspace` must already be canonical (see [`canonical_workspace`]).
    ///
    /// Adding a folder that is already a project returns the existing record
    /// rather than a second one. Two projects over one folder would share a
    /// workspace root, and therefore every path grant made against it — a
    /// duplicate is never what the user meant, and silently reusing the
    /// original is the behaviour that cannot surprise them.
    pub fn create(&self, name: &str, workspace: &Path) -> AppResult<Project> {
        let workspace_path = path_to_string(workspace)?;
        let mut projects = self.projects();

        if let Some(existing) = projects.iter().find(|p| p.workspace_path == workspace_path) {
            tracing::debug!(id = %existing.id, "workspace is already a project");
            return Ok(existing.to_project());
        }

        let name = match name.trim() {
            "" => default_name(workspace),
            trimmed => trimmed.to_owned(),
        };

        let project = StoredProject {
            id: Uuid::new_v4().to_string(),
            name,
            workspace_path,
            created_at: now(),
            last_opened_at: None,
        };
        let created = project.to_project();

        projects.push(project);
        self.save(&projects)?;

        tracing::info!(id = %created.id, name = %created.name, "project created");
        Ok(created)
    }

    /// Opens a project, stamping `last_opened_at`.
    ///
    /// The stamp is what orders the sidebar, so it is written here rather than
    /// left to the UI: opening a project is the act that makes it recent.
    pub fn open(&self, id: &str) -> AppResult<ProjectDetail> {
        let mut projects = self.projects();

        let project = projects
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| AppError::ProjectNotFound { id: id.to_owned() })?;

        project.last_opened_at = Some(now());
        let opened = project.to_project();

        self.save(&projects)?;

        tracing::debug!(id, exists = opened.workspace_exists, "project opened");
        Ok(ProjectDetail {
            project: opened,
            // Phase 5 fills this in.
            sessions: Vec::new(),
        })
    }

    /// Forgets a project. The workspace folder itself is never touched.
    pub fn delete(&self, id: &str) -> AppResult<()> {
        let mut projects = self.projects();

        let before = projects.len();
        projects.retain(|p| p.id != id);
        if projects.len() == before {
            return Err(AppError::ProjectNotFound { id: id.to_owned() });
        }

        self.save(&projects)?;

        tracing::info!(id, "project deleted");
        Ok(())
    }

    /// Serializes the list and replaces the document atomically.
    ///
    /// Takes the guard so a caller cannot mutate the list and forget to
    /// persist it: the only way to reach this is to already hold the lock.
    fn save(&self, projects: &[StoredProject]) -> AppResult<()> {
        let file = ProjectsFile {
            version: SCHEMA_VERSION,
            projects: projects.to_vec(),
        };

        let bytes = serde_json::to_vec_pretty(&file).map_err(|err| {
            tracing::error!(%err, "could not serialize the project store");
            AppError::Store {
                action: "serialize",
                source: io::Error::other(err),
            }
        })?;

        write_atomic(&self.path, &bytes).map_err(|err| {
            tracing::error!(%err, path = %self.path.display(), "could not save the project store");
            AppError::Store {
                action: "save",
                source: err,
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Paths and timestamps
// ---------------------------------------------------------------------------

/// Resolves a user-supplied workspace path to a canonical absolute directory.
///
/// `dunce::canonicalize` is `fs::canonicalize` without the Windows verbatim
/// prefix: the plain `fs` version returns `\\?\C:\work`, which is correct but
/// unreadable in a UI and compares unequal to every path the user will ever
/// type. Symlinks are still resolved, so the stored path is the real location
/// of the folder rather than a link that may later point elsewhere.
///
/// This is the whole of Aegis' path handling for now. Phase 3 introduces
/// `policy/path.rs` as the single owner of resolution and workspace
/// containment; this helper moves there rather than being duplicated.
pub fn canonical_workspace(raw: &str) -> AppResult<PathBuf> {
    let trimmed = raw.trim();
    let invalid = |reason: &str| AppError::WorkspacePath {
        path: trimmed.to_owned(),
        reason: reason.to_owned(),
    };

    if trimmed.is_empty() {
        return Err(invalid("no folder was given"));
    }
    if !Path::new(trimmed).is_absolute() {
        return Err(invalid("the path is not absolute"));
    }

    let canonical = dunce::canonicalize(trimmed).map_err(|err| {
        invalid(match err.kind() {
            io::ErrorKind::NotFound => "the folder does not exist",
            io::ErrorKind::PermissionDenied => "the folder is not readable",
            _ => "the folder could not be resolved",
        })
    })?;

    if !canonical.is_dir() {
        return Err(invalid("that is a file, not a folder"));
    }

    Ok(canonical)
}

/// A canonical path as a string, or a structured error.
///
/// Paths are OS strings and need not be UTF-8. Rejecting the handful that are
/// not is better than lossily converting one and storing a path that no longer
/// opens the folder it names.
fn path_to_string(path: &Path) -> AppResult<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| AppError::WorkspacePath {
            path: path.to_string_lossy().into_owned(),
            reason: "the path is not valid UTF-8".to_owned(),
        })
}

/// The folder's own name, used when the UI supplies no project name.
///
/// A drive or filesystem root has no file name; naming the project after the
/// root itself (`C:\`, `/`) is clearer there than an empty label.
fn default_name(workspace: &Path) -> String {
    workspace
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| workspace.to_string_lossy().into_owned())
}

/// Now, as fixed-width UTC RFC3339 (`2026-08-28T09:41:07.412Z`).
///
/// Fixed width and a fixed offset are what let timestamps be compared as
/// strings, both here and in the UI.
fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Drops a leading UTF-8 byte-order mark.
///
/// JSON has no BOM, and `serde_json` rejects one outright. Windows editors —
/// Notepad, and PowerShell's `Set-Content -Encoding utf8` — write one anyway,
/// so a user who takes up the invitation to edit `projects.json` by hand would
/// otherwise watch their project list get quarantined for a change they cannot
/// see. Aegis never writes a BOM; it only tolerates one.
fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)
}

/// Moves a document aside so a fresh one can be written.
///
/// Best effort by design: the caller is already on the "the store is
/// unusable" path, and failing to rename it must not stop the app from
/// starting. The original is kept rather than deleted — it is the only copy of
/// the user's project list, and a human may well be able to repair it.
fn quarantine(path: &Path) {
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let backup = path.with_extension(format!("corrupt-{stamp}.json"));

    match fs::rename(path, &backup) {
        Ok(()) => tracing::warn!(backup = %backup.display(), "damaged project store moved aside"),
        Err(err) => tracing::error!(%err, "could not move the damaged project store aside"),
    }
}

/// Writes `bytes` to `path` so that readers see either the old file or the
/// whole new one.
///
/// Temporary file in the same directory (a rename across filesystems is not
/// atomic), `sync_all` before the rename (a rename can otherwise outrun the
/// data and survive a crash pointing at empty content), then a replacing
/// rename. The temporary file is removed if the rename never succeeds, so a
/// failing store does not leave litter behind.
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

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    /// A store plus the directories it and its workspaces live in.
    struct Fixture {
        _dir: TempDir,
        data: PathBuf,
        store: Store,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = TempDir::new().expect("temp dir");
            let data = dir.path().join("data");
            let store = Store::load(&data);
            Self {
                _dir: dir,
                data,
                store,
            }
        }

        /// Reopens the store from the same directory, as a restart would.
        fn reload(&self) -> Store {
            Store::load(&self.data)
        }

        fn document(&self) -> PathBuf {
            self.data.join(PROJECTS_FILE)
        }

        /// Creates a real folder to use as a workspace.
        fn workspace(&self, name: &str) -> PathBuf {
            let path = self._dir.path().join(name);
            fs::create_dir_all(&path).expect("workspace dir");
            dunce::canonicalize(&path).expect("canonical workspace")
        }
    }

    /// The field names in `src/ipc/bindings.ts` are hand-written until Phase 5
    /// generates them. This is what makes that safe: renaming a field in Rust
    /// without renaming it there fails here rather than in the UI.
    #[test]
    fn payloads_carry_the_documented_field_names() {
        let detail = ProjectDetail {
            project: Project {
                id: "p".to_owned(),
                name: "Alpha".to_owned(),
                workspace_path: "/w".to_owned(),
                created_at: "2026-08-28T09:41:07.412Z".to_owned(),
                last_opened_at: None,
                workspace_exists: true,
            },
            sessions: vec![SessionSummary {
                id: "s".to_owned(),
                project_id: "p".to_owned(),
                title: "First".to_owned(),
                created_at: "2026-08-28T09:41:07.412Z".to_owned(),
                updated_at: "2026-08-28T09:41:07.412Z".to_owned(),
                message_count: 3,
                state: SessionState::AwaitingApproval,
            }],
        };

        let json = serde_json::to_value(&detail).expect("ProjectDetail serializes");

        // Compared as sets: serde_json's key order depends on a feature flag a
        // dependency could flip, and the contract is the names, not the order.
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

        assert_eq!(fields(&json), sorted(&["project", "sessions"]));
        assert_eq!(
            fields(&json["project"]),
            sorted(&[
                "id",
                "name",
                "workspace_path",
                "created_at",
                "last_opened_at",
                "workspace_exists",
            ])
        );
        assert_eq!(
            fields(&json["sessions"][0]),
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

        // `None` must reach TypeScript as `null`, not as a missing key: the
        // binding declares `string | null`.
        assert!(json["project"]["last_opened_at"].is_null());
        assert_eq!(json["sessions"][0]["state"], "awaiting_approval");
    }

    #[test]
    fn starts_empty_without_a_document() {
        let fx = Fixture::new();

        assert!(fx.store.list().is_empty());
        assert!(
            !fx.document().exists(),
            "loading must not write; only a mutation does"
        );
    }

    #[test]
    fn a_project_survives_a_restart() {
        let fx = Fixture::new();
        let workspace = fx.workspace("alpha");

        let created = fx.store.create("Alpha", &workspace).expect("create");
        assert_eq!(created.name, "Alpha");
        assert_eq!(created.workspace_path, workspace.to_string_lossy());
        assert!(created.workspace_exists);
        assert_eq!(created.last_opened_at, None);

        let reloaded = fx.reload().list();
        assert_eq!(reloaded, vec![created]);
    }

    #[test]
    fn the_same_workspace_never_becomes_two_projects() {
        let fx = Fixture::new();
        let workspace = fx.workspace("alpha");

        let first = fx.store.create("Alpha", &workspace).expect("create");
        let second = fx
            .store
            .create("Alpha again", &workspace)
            .expect("create again");

        assert_eq!(first.id, second.id, "the original record is returned");
        assert_eq!(second.name, "Alpha", "and its name is left alone");
        assert_eq!(fx.store.list().len(), 1);
    }

    #[test]
    fn an_empty_name_falls_back_to_the_folder_name() {
        let fx = Fixture::new();
        let workspace = fx.workspace("some-repo");

        let created = fx.store.create("   ", &workspace).expect("create");

        assert_eq!(created.name, "some-repo");
    }

    #[test]
    fn opening_stamps_the_project_and_persists_it() {
        let fx = Fixture::new();
        let workspace = fx.workspace("alpha");
        let created = fx.store.create("Alpha", &workspace).expect("create");

        let detail = fx.store.open(&created.id).expect("open");

        assert!(detail.project.last_opened_at.is_some());
        assert!(detail.sessions.is_empty(), "sessions arrive in Phase 5");

        let after_restart = fx.reload().list();
        assert_eq!(
            after_restart.first().and_then(|p| p.last_opened_at.clone()),
            detail.project.last_opened_at
        );
    }

    #[test]
    fn the_list_puts_the_most_recently_opened_first() {
        let fx = Fixture::new();
        let alpha = fx.store.create("Alpha", &fx.workspace("alpha")).expect("a");
        let beta = fx.store.create("Beta", &fx.workspace("beta")).expect("b");

        // Newest-created first, before anything has been opened.
        assert_eq!(fx.store.list()[0].id, beta.id);

        fx.store.open(&alpha.id).expect("open alpha");

        let listed = fx.store.list();
        assert_eq!(listed[0].id, alpha.id, "opening moves a project to the top");
        assert_eq!(listed[1].id, beta.id);
    }

    #[test]
    fn a_missing_workspace_is_reported_but_the_project_is_kept() {
        let fx = Fixture::new();
        let workspace = fx.workspace("gone");
        let created = fx.store.create("Gone", &workspace).expect("create");
        assert!(created.workspace_exists);

        fs::remove_dir_all(&workspace).expect("remove workspace");

        let listed = fx.store.list();
        assert_eq!(listed.len(), 1, "the project outlives its folder");
        assert!(!listed[0].workspace_exists);

        // And opening it still works, so the UI can explain the problem.
        let detail = fx.store.open(&created.id).expect("open");
        assert!(!detail.project.workspace_exists);
    }

    #[test]
    fn deleting_removes_only_the_named_project() {
        let fx = Fixture::new();
        let alpha = fx.store.create("Alpha", &fx.workspace("alpha")).expect("a");
        let beta = fx.store.create("Beta", &fx.workspace("beta")).expect("b");

        fx.store.delete(&alpha.id).expect("delete");

        let listed = fx.reload().list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, beta.id);
    }

    #[test]
    fn unknown_ids_are_rejected_rather_than_ignored() {
        let fx = Fixture::new();

        let opened = fx.store.open("nope").expect_err("open must fail");
        let deleted = fx.store.delete("nope").expect_err("delete must fail");

        assert!(matches!(opened, AppError::ProjectNotFound { .. }));
        assert!(matches!(deleted, AppError::ProjectNotFound { .. }));
    }

    /// Windows editors write a BOM; the store must read what they produce.
    #[test]
    fn a_hand_edited_document_with_a_byte_order_mark_still_loads() {
        let fx = Fixture::new();
        let workspace = fx.workspace("alpha");
        fx.store.create("Alpha", &workspace).expect("create");

        let body = fs::read(fx.document()).expect("read document");
        let mut with_bom = vec![0xEF, 0xBB, 0xBF];
        with_bom.extend_from_slice(&body);
        fs::write(fx.document(), &with_bom).expect("rewrite with a BOM");

        let listed = fx.reload().list();
        assert_eq!(listed.len(), 1, "a BOM must not look like corruption");
        assert_eq!(listed[0].name, "Alpha");
    }

    #[test]
    fn a_damaged_document_is_moved_aside_instead_of_blocking_startup() {
        let fx = Fixture::new();
        fx.store
            .create("Alpha", &fx.workspace("alpha"))
            .expect("create");

        fs::write(fx.document(), b"{ this is not json").expect("corrupt the store");

        let recovered = fx.reload();
        assert!(recovered.list().is_empty(), "the app still starts");

        let quarantined: Vec<_> = fs::read_dir(&fx.data)
            .expect("read data dir")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains("corrupt-"))
            .collect();
        assert_eq!(quarantined.len(), 1, "the original is kept, not deleted");
    }

    #[test]
    fn a_future_schema_version_is_quarantined_rather_than_guessed_at() {
        let fx = Fixture::new();
        fs::create_dir_all(&fx.data).expect("data dir");
        fs::write(
            fx.document(),
            br#"{"version":99,"projects":[{"id":"x","name":"X","workspace_path":"/tmp","created_at":"2026-01-01T00:00:00.000Z"}]}"#,
        )
        .expect("write future document");

        assert!(fx.reload().list().is_empty());
        assert!(!fx.document().exists(), "the unknown document was moved");
    }

    #[test]
    fn saving_leaves_no_temporary_file_behind() {
        let fx = Fixture::new();
        fx.store
            .create("Alpha", &fx.workspace("alpha"))
            .expect("create");

        let leftovers: Vec<_> = fs::read_dir(&fx.data)
            .expect("read data dir")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "found {leftovers:?}");
    }

    #[test]
    fn workspace_exists_is_never_written_to_disk() {
        let fx = Fixture::new();
        fx.store
            .create("Alpha", &fx.workspace("alpha"))
            .expect("create");

        let raw = fs::read_to_string(fx.document()).expect("read document");
        assert!(
            !raw.contains("workspace_exists"),
            "a stale copy on disk would outlive the folder it describes"
        );
        assert!(raw.contains("\"version\": 1"));
    }

    #[test]
    fn canonical_workspace_resolves_a_real_folder() {
        let dir = TempDir::new().expect("temp dir");
        let nested = dir.path().join("a").join("b");
        fs::create_dir_all(&nested).expect("nested dirs");

        let messy = dir.path().join("a").join("b").join("..").join("b");
        let resolved = canonical_workspace(&messy.to_string_lossy()).expect("canonicalize");

        assert_eq!(resolved, dunce::canonicalize(&nested).expect("expected"));
        assert!(
            !resolved.to_string_lossy().starts_with(r"\\?\"),
            "verbatim prefixes must not reach the UI: {}",
            resolved.display()
        );
    }

    #[test]
    fn canonical_workspace_refuses_what_is_not_a_workspace() {
        let dir = TempDir::new().expect("temp dir");
        let file = dir.path().join("a-file.txt");
        fs::write(&file, b"x").expect("write file");

        let cases = [
            ("", "no folder was given"),
            ("   ", "no folder was given"),
            ("relative/path", "the path is not absolute"),
        ];
        for (input, reason) in cases {
            match canonical_workspace(input) {
                Err(AppError::WorkspacePath { reason: got, .. }) => assert_eq!(got, reason),
                other => panic!("{input:?} gave {other:?}"),
            }
        }

        let missing = dir.path().join("not-there");
        assert!(matches!(
            canonical_workspace(&missing.to_string_lossy()),
            Err(AppError::WorkspacePath { .. })
        ));
        assert!(matches!(
            canonical_workspace(&file.to_string_lossy()),
            Err(AppError::WorkspacePath { .. })
        ));
    }
}
