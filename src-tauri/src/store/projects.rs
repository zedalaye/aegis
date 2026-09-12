//! The project document: `projects.json`.
//!
//! A project is a workspace folder plus a name. From Phase 3 it is also the
//! root every path check is measured against, which is why the path is
//! canonicalized on the way in and stored canonical — everything downstream
//! compares against a resolved path rather than against whatever string the UI
//! happened to hold.
//!
//! The atomic write, the quarantine and the timestamp format all live in the
//! parent module, shared with [`sessions`](super::sessions).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use super::sessions::SessionSummary;
use super::{now, quarantine, strip_bom, write_atomic};
use crate::error::{AppError, AppResult};
use crate::exec_host::{self, ExecHost};

/// Name of the document under the application-data directory.
const PROJECTS_FILE: &str = "projects.json";

/// Schema version of [`ProjectsFile`].
///
/// A document carrying anything else is treated exactly like a damaged one:
/// quarantined, not guessed at. Bumping this is how a future migration
/// announces itself.
const SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// IPC payloads (PLAN 2.1, "Projects")
// ---------------------------------------------------------------------------

/// A project as the UI sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
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
    /// The WSL distribution whose filesystem the folder is in, if any.
    ///
    /// Derived from the path on every read, like `workspace_exists`, and never
    /// stored — it is a fact about where the folder is, not a decision anybody
    /// made. It is emphatically **not** `exec_host` and never sets it: PLAN
    /// 7.12 forbids flipping the host from a `\\wsl$\` path, because picking a
    /// folder is not consent. All it does is let the picker mark the row a
    /// person is most likely to want, which still takes their click.
    pub workspace_distro: Option<String>,
    /// Where this project's commands run (PLAN 7.12).
    ///
    /// `None` is this process — the default, what every project had before this
    /// slice, and what a project keeps unless somebody chooses otherwise.
    /// Sessions inherit it; they do not override it, because "which operating
    /// system does the toolchain live in" is a fact about the folder rather
    /// than about a conversation in it.
    pub exec_host: Option<ExecHost>,
}

/// What opening a project yields: the project plus its sessions, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct ProjectDetail {
    pub project: Project,
    /// The project's sessions, newest first.
    ///
    /// Filled by the command layer rather than by [`Store::open`]: a summary
    /// carries the session's *live* state, and only [`AppState`] can see both
    /// the session document and the turns currently running.
    ///
    /// [`AppState`]: crate::state::AppState
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
    /// Where this project's commands run (PLAN 7.12).
    ///
    /// Absent rather than null when there is none, and `#[serde(default)]` on
    /// the way in: every row written before this slice has no such field, and a
    /// document those rows still round-trip through unchanged is what makes
    /// "existing projects keep working" a property rather than a hope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exec_host: Option<ExecHost>,
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
            workspace_distro: exec_host::distro_of(Path::new(&self.workspace_path)),
            exec_host: self.exec_host.clone(),
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

    /// One project by id, without touching `last_opened_at`.
    ///
    /// Deliberately not [`Store::open`]: a command that only needs to know
    /// where a project's folder is — where its shared files live, say — is not
    /// the user opening it, and stamping recency for it would reorder the
    /// sidebar behind their back.
    pub fn get(&self, id: &str) -> AppResult<Project> {
        self.projects()
            .iter()
            .find(|p| p.id == id)
            .map(StoredProject::to_project)
            .ok_or_else(|| AppError::ProjectNotFound { id: id.to_owned() })
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
            exec_host: None,
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
            // Left empty here and filled by the command, which can see the
            // session document and the running turns at the same time. The
            // project store has no business knowing either.
            sessions: Vec::new(),
        })
    }

    /// Says where this project's commands run, or clears it (PLAN 7.12).
    ///
    /// `None` puts the project back on this process, which is where every
    /// project starts. Whether the host is one this machine can actually use is
    /// settled by the command above this — the store's job is to remember what
    /// was chosen, and a store that also validated would be a second opinion
    /// that can disagree with the first.
    ///
    /// Deliberately not part of `open`: choosing a host is a decision somebody
    /// makes once, and stamping recency for it would reorder the sidebar
    /// underneath them.
    pub fn set_exec_host(&self, id: &str, host: Option<ExecHost>) -> AppResult<Project> {
        let mut projects = self.projects();

        let project = projects
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| AppError::ProjectNotFound { id: id.to_owned() })?;

        project.exec_host = host;
        let updated = project.to_project();

        self.save(&projects)?;

        tracing::info!(id, host = ?updated.exec_host, "execution host set");
        Ok(updated)
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

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    use crate::store::sessions::{Cost, SessionState};

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

    /// `src/ipc/bindings.ts` is generated from these structs, so a renamed
    /// field reaches TypeScript on its own. What generation cannot check is
    /// that the names still match the contract in `PLAN.md` § 2.1 — a rename
    /// would regenerate happily and silently change the wire format. This test
    /// is that check, and it is why it lists the names literally.
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
                workspace_distro: None,
                exec_host: None,
            },
            sessions: vec![SessionSummary {
                agent_id: crate::store::DEFAULT_AGENT_ID.to_owned(),
                id: "s".to_owned(),
                project_id: "p".to_owned(),
                title: "First".to_owned(),
                created_at: "2026-08-28T09:41:07.412Z".to_owned(),
                updated_at: "2026-08-28T09:41:07.412Z".to_owned(),
                message_count: 3,
                state: SessionState::AwaitingApproval,
                delegated: None,
                scheduled: None,
                cost: Cost::default(),
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
                "workspace_distro",
                "exec_host",
            ])
        );
        assert_eq!(
            fields(&json["sessions"][0]),
            sorted(&[
                "id",
                "project_id",
                "agent_id",
                "title",
                "created_at",
                "updated_at",
                "message_count",
                "state",
                "delegated",
                "scheduled",
                "cost",
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

    /// Looking a project up is not the user opening it, so the sidebar's order
    /// must be exactly where it was afterwards.
    #[test]
    fn getting_a_project_does_not_make_it_recent() {
        let fx = Fixture::new();
        let alpha = fx.store.create("Alpha", &fx.workspace("alpha")).expect("a");
        let beta = fx.store.create("Beta", &fx.workspace("beta")).expect("b");
        fx.store.open(&beta.id).expect("open");

        let got = fx.store.get(&alpha.id).expect("get");

        assert_eq!(got.name, "Alpha");
        assert_eq!(got.last_opened_at, None);
        assert_eq!(
            fx.store.list().first().map(|p| p.id.clone()),
            Some(beta.id),
            "the order is untouched"
        );
    }

    #[test]
    fn unknown_ids_are_rejected_rather_than_ignored() {
        let fx = Fixture::new();

        let opened = fx.store.open("nope").expect_err("open must fail");
        let got = fx.store.get("nope").expect_err("get must fail");
        let deleted = fx.store.delete("nope").expect_err("delete must fail");

        assert!(matches!(opened, AppError::ProjectNotFound { .. }));
        assert!(matches!(got, AppError::ProjectNotFound { .. }));
        assert!(matches!(deleted, AppError::ProjectNotFound { .. }));
    }

    /// A host is chosen once and has to still be there next week, which is
    /// the whole reason it lives on the project rather than on a session.
    #[test]
    fn an_execution_host_survives_a_restart_and_can_be_taken_back() {
        let fx = Fixture::new();
        let created = fx
            .store
            .create("Alpha", &fx.workspace("alpha"))
            .expect("create");
        assert_eq!(created.exec_host, None, "a project starts on this computer");

        let host = ExecHost::Wsl {
            distro: "Ubuntu".to_owned(),
        };
        let set = fx
            .store
            .set_exec_host(&created.id, Some(host.clone()))
            .expect("set");
        assert_eq!(set.exec_host, Some(host.clone()));

        assert_eq!(
            fx.reload().list().first().and_then(|p| p.exec_host.clone()),
            Some(host)
        );

        fx.store.set_exec_host(&created.id, None).expect("clear");
        assert_eq!(
            fx.reload().list().first().and_then(|p| p.exec_host.clone()),
            None,
            "clearing puts the project back on this computer"
        );
    }

    /// The rule that lets this field be added to a document already on
    /// someone's disk: a row written before it existed still loads, and a
    /// project with no host writes no field at all — so the two documents are
    /// the same bytes and a downgrade loses nothing.
    #[test]
    fn rows_from_before_the_host_existed_still_load() {
        let fx = Fixture::new();
        fs::create_dir_all(&fx.data).expect("data dir");
        fs::write(
            fx.document(),
            br#"{"version":1,"projects":[{"id":"x","name":"X","workspace_path":"/tmp","created_at":"2026-01-01T00:00:00.000Z"}]}"#,
        )
        .expect("write an older document");

        let listed = fx.reload().list();
        assert_eq!(listed.len(), 1, "the row is read, not quarantined");
        assert_eq!(listed[0].exec_host, None);

        fx.store
            .create("Alpha", &fx.workspace("alpha"))
            .expect("create");
        let raw = fs::read_to_string(fx.document()).expect("read document");
        assert!(
            !raw.contains("exec_host"),
            "a project on this computer writes no host: {raw}"
        );
    }

    #[test]
    fn setting_a_host_on_an_unknown_project_is_refused() {
        let fx = Fixture::new();

        let refused = fx
            .store
            .set_exec_host("nope", None)
            .expect_err("set must fail");

        assert!(matches!(refused, AppError::ProjectNotFound { .. }));
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
