//! Workspace versioning (PLAN 7.11).
//!
//! A scaffolded workspace is a git work tree, so the rewritten-in-place board
//! has history — for any folder, not just code (PLAN 7.1).
//!
//! * [`measure`] walks up for a `.git` without running a program: cheap enough
//!   for every render, and correct without `git` installed.
//! * [`ensure`] runs `git init` only from
//!   [`workspace_scaffold`](crate::commands::workspace::workspace_scaffold) —
//!   the button is the consent — and never inside an existing work tree (no
//!   nested repositories).
//!
//! **Whose `git`**: the project's execution host (PLAN 7.12). For WSL,
//! `git init` goes through `wsl.exe --exec` after a `test -d` probe, since
//! `wsl.exe --cd` silently falls back to the home directory.
//!
//! **Not a git client**: no commit on a branch, remote, `.gitignore` or
//! identity is ever written. Commits are `shell_exec` of `git` under the gate
//! (grant rules: PLAN 3.1). The one thing written is a run's
//! [`checkpoint`] on a side ref (PLAN 7.24).

pub mod checkpoint;

use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use serde::Serialize;
use tokio::process::Command;
use ts_rs::TS;

use crate::exec_host::{self, ExecHost};
use crate::tools::shell;

/// The entry that marks a work tree — a file in worktrees and submodules, so
/// only existence is checked.
const GIT_ENTRY: &str = ".git";

/// The program, by the name PATH knows it under.
const GIT: &str = "git";

/// How long a distribution has to answer (same as `shell_exec`'s probe; a cold
/// WSL VM takes seconds).
const HOST_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// IPC payloads
// ---------------------------------------------------------------------------

/// Whether the folder is in a work tree, and whose. An ancestor's counts:
/// [`ensure`] must not split its history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum WorkTree {
    /// The workspace root holds the `.git`. Its own repository.
    Here,
    /// A folder above it holds the `.git`. Versioned, by somebody else's repo.
    Ancestor,
    /// Nothing from the workspace root to the top of the disk holds one.
    Unversioned,
}

impl WorkTree {
    /// Whether the files in this folder have a history at all.
    pub const fn is_versioned(self) -> bool {
        !matches!(self, Self::Unversioned)
    }
}

/// Where the history of a workspace's files is kept, if anywhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct Versioning {
    /// Whether the workspace is in a work tree, and whose.
    pub tree: WorkTree,
    /// The folder holding the `.git`, absolute, or `None`.
    pub at: Option<String>,
}

impl Versioning {
    /// The unversioned answer.
    const fn none() -> Self {
        Self {
            tree: WorkTree::Unversioned,
            at: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Read
// ---------------------------------------------------------------------------

/// Where the `.git` for this folder is, by a filesystem walk rather than
/// `git rev-parse`. Unlike git it ignores `GIT_CEILING_DIRECTORIES` and mount
/// boundaries; the report names the ancestor, so that case is visible.
pub fn measure(root: &Path) -> Versioning {
    for (up, dir) in root.ancestors().enumerate() {
        if !dir.join(GIT_ENTRY).exists() {
            continue;
        }
        return Versioning {
            tree: if up == 0 {
                WorkTree::Here
            } else {
                WorkTree::Ancestor
            },
            at: Some(dir.display().to_string()),
        };
    }
    Versioning::none()
}

// ---------------------------------------------------------------------------
// Write
// ---------------------------------------------------------------------------

/// What one scaffolding run did about versioning, spread onto
/// [`ScaffoldReport`](crate::workspace::ScaffoldReport).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ensured {
    /// Where the history is, measured *after* the run.
    pub versioning: Versioning,
    /// Whether this run is what created it.
    pub initialized: bool,
    /// Why the folder is still not in a work tree, when it is not.
    pub problem: Option<String>,
}

/// Makes the workspace a work tree if it is not already one.
///
/// Already a work tree (here or above): untouched. Otherwise a bare `git init`
/// on `host` (never inferred from the path, PLAN 7.12). If `git` is
/// unreachable, the reason goes in the report. Never fails.
pub async fn ensure(root: &Path, host: Option<&ExecHost>) -> Ensured {
    let found = measure(root);
    if found.tree.is_versioned() {
        tracing::debug!(
            root = %root.display(),
            at = ?found.at,
            "the workspace is already versioned"
        );
        return Ensured {
            versioning: found,
            initialized: false,
            problem: None,
        };
    }

    match init(root, host).await {
        Ok(()) => {
            tracing::info!(root = %root.display(), "the workspace is now a git work tree");
            Ensured {
                // Re-measured rather than assumed: the report is a claim about
                // what is on disk, and `git init` reporting success is not
                // quite the same statement as `.git` being there.
                versioning: measure(root),
                initialized: true,
                problem: None,
            }
        }
        Err(problem) => {
            tracing::info!(
                root = %root.display(),
                %problem,
                "the workspace was left unversioned"
            );
            Ensured {
                versioning: found,
                initialized: false,
                problem: Some(problem),
            }
        }
    }
}

/// Runs `git init` in `root`, or says in one line why it could not.
///
/// Spawned directly: the button press is the approval. No arguments, so the
/// user's `init.defaultBranch` and `init.templateDir` apply.
async fn init(root: &Path, host: Option<&ExecHost>) -> Result<(), String> {
    let mut command = match host {
        None => here(root)?,
        Some(ExecHost::Wsl { distro }) => inside(distro, root).await?,
    };

    command
        // A `git` that decides to ask something must not sit waiting on a
        // terminal nobody is attached to, and what it prints belongs in the
        // failure line rather than in whatever console started this process.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let output = answered(command.output(), "`git init`")
        .await?
        .map_err(|err| format!("`git init` would not start ({err})"))?;

    if output.status.success() {
        return Ok(());
    }
    Err(failure(&output.status, &output.stderr))
}

/// `git init`, run by this computer's own `git`.
fn here(root: &Path) -> Result<Command, String> {
    let program = program(root)?;

    let mut command = Command::new(&program);
    command.arg("init").current_dir(root);
    no_window(&mut command);
    Ok(command)
}

/// `git init`, run by a WSL distribution's `git` (PLAN 7.12).
///
/// `--exec` like `shell_exec` (no login shell), after a `test -d` probe:
/// `wsl.exe --cd` silently starts in `~` for a missing folder.
#[cfg(windows)]
async fn inside(distro: &str, root: &Path) -> Result<Command, String> {
    let wsl = exec_host::wsl_exe().ok_or_else(|| {
        format!(
            "this project runs its commands in `{distro}`, and `wsl.exe` is not on this machine"
        )
    })?;
    let cwd = exec_host::linux_path(distro, root)?;

    let mut probe = Command::new(&wsl);
    probe
        .args(["-d", distro, "--exec", "test", "-d", &cwd])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    no_window(&mut probe);

    let asked = answered(probe.output(), &format!("`{distro}`"))
        .await?
        .map_err(|err| {
            format!("`wsl.exe` would not start, so `{distro}` could not be reached ({err})")
        })?;

    match asked.status.code() {
        Some(0) => {}
        // `test` said no. The distribution is there; the folder is not.
        Some(1) => {
            return Err(format!(
                "`{cwd}` is not a folder inside `{distro}`, so nothing was initialised there"
            ))
        }
        // WSL's own refusal — no such distribution, the service is not running,
        // the virtual machine would not start. Its words, because they name
        // which of those it was.
        _ => {
            let said = exec_host::message(&asked.stderr);
            let said = if said.is_empty() {
                String::new()
            } else {
                format!(": {said}")
            };
            return Err(format!(
                "`{distro}` is not a distribution a command can be run in right now{said}"
            ));
        }
    }

    let mut command = Command::new(&wsl);
    // Deliberately no `current_dir`: `--cd` has already said where this starts,
    // and a Windows working directory would be a second answer to the same
    // question.
    command.args(["-d", distro, "--cd", &cwd, "--exec", GIT, "init"]);
    no_window(&mut command);
    Ok(command)
}

/// `git init` in a WSL distribution. Never, off Windows.
///
/// Reachable via a `projects.json` from Windows; refused rather than using the
/// local `git`.
#[cfg(not(windows))]
async fn inside(distro: &str, _root: &Path) -> Result<Command, String> {
    Err(format!(
        "this project runs its commands in `{distro}`, and this computer has no WSL"
    ))
}

/// Waits for a child with a timeout, or says what stopped answering.
async fn answered<T>(work: impl std::future::Future<Output = T>, what: &str) -> Result<T, String> {
    tokio::time::timeout(HOST_TIMEOUT, work).await.map_err(|_| {
        format!(
            "{what} did not answer within {} seconds, so the folder was left unversioned",
            HOST_TIMEOUT.as_secs()
        )
    })
}

/// Keeps a GUI-launched app from flashing a console window (PLAN 5.1).
///
/// The same flag `shell_exec` and the MCP client already spawn with.
fn no_window(command: &mut Command) {
    #[cfg(windows)]
    {
        // `tokio::process::Command` carries this natively on Windows.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = command;
    }
}

/// Why a `git init` that ran came back unhappy, in one line.
fn failure(status: &ExitStatus, stderr: &[u8]) -> String {
    let said = String::from_utf8_lossy(stderr);
    let first = said
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();

    if first.is_empty() {
        match status.code() {
            Some(code) => format!("`git init` exited with {code}"),
            None => "`git init` was killed".to_owned(),
        }
    } else {
        format!("`git init` said: {first}")
    }
}

/// Where `git` is on this machine.
///
/// PATH first (the resolver `shell_exec` uses), then common install locations,
/// since a GUI-launched app may lack the login shell's PATH.
fn program(root: &Path) -> Result<PathBuf, String> {
    if let Ok(found) = shell::resolve(GIT, root) {
        return Ok(found);
    }

    installed()
        .into_iter()
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| "`git` is not on this machine's PATH".to_owned())
}

/// Where an installer puts `git` when PATH has not been told about it.
#[cfg(windows)]
fn installed() -> Vec<PathBuf> {
    // `cmd\git.exe` rather than `bin\git.exe`: Git for Windows' `cmd` directory
    // is the one its own installer puts on PATH, and the `bin` one drags the
    // MSYS runtime in front of everything else.
    ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|dir| PathBuf::from(dir).join("Git").join("cmd").join("git.exe"))
        .chain(std::env::var_os("LOCALAPPDATA").map(|dir| {
            PathBuf::from(dir)
                .join("Programs")
                .join("Git")
                .join("cmd")
                .join("git.exe")
        }))
        .collect()
}

/// Where an installer puts `git` when PATH has not been told about it.
#[cfg(target_os = "macos")]
fn installed() -> Vec<PathBuf> {
    // `/usr/bin/git` is always on a GUI app's PATH and so is never reached
    // here. Homebrew's two prefixes are the ones a `launchd`-started process
    // has never heard of.
    vec![
        PathBuf::from("/opt/homebrew/bin/git"),
        PathBuf::from("/usr/local/bin/git"),
    ]
}

/// Where an installer puts `git` when PATH has not been told about it.
#[cfg(not(any(windows, target_os = "macos")))]
fn installed() -> Vec<PathBuf> {
    // A desktop session's PATH holds `/usr/bin` and `/usr/local/bin` on every
    // distribution that has a desktop session, so there is nothing to add.
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// A canonical, empty folder to measure.
    fn folder() -> (TempDir, PathBuf) {
        let dir = TempDir::new().expect("temp dir");
        let root = dir.path().join("work");
        fs::create_dir_all(&root).expect("work dir");
        let root = dunce::canonicalize(&root).expect("canonical");
        (dir, root)
    }

    /// Whether git is available. Tests needing it skip without it;
    /// [`a_missing_git_is_a_reported_problem`] runs only when it is absent.
    fn has_git(root: &Path) -> bool {
        program(root).is_ok()
    }

    #[test]
    fn an_ordinary_folder_is_unversioned() {
        let (_dir, root) = folder();

        assert_eq!(measure(&root), Versioning::none());
        assert!(!measure(&root).tree.is_versioned());
    }

    #[test]
    fn a_dot_git_in_the_folder_itself_is_here() {
        let (_dir, root) = folder();
        fs::create_dir(root.join(GIT_ENTRY)).expect("a repository");

        let found = measure(&root);

        assert_eq!(found.tree, WorkTree::Here);
        assert_eq!(found.at, Some(root.display().to_string()));
    }

    /// A linked worktree and a submodule both carry `.git` as a *file*. Asking
    /// whether it is a directory would call both of them unversioned.
    #[test]
    fn a_dot_git_file_counts_as_much_as_a_directory() {
        let (_dir, root) = folder();
        fs::write(root.join(GIT_ENTRY), "gitdir: ../.git/worktrees/work\n").expect("a linked tree");

        assert_eq!(measure(&root).tree, WorkTree::Here);
    }

    /// The case that makes this three answers and not a boolean: a folder
    /// inside somebody's monorepo already has a history.
    #[test]
    fn a_repository_above_the_folder_is_an_ancestor() {
        let (dir, root) = folder();
        let above = dunce::canonicalize(dir.path()).expect("canonical");
        fs::create_dir(above.join(GIT_ENTRY)).expect("a repository above");

        let found = measure(&root);

        assert_eq!(found.tree, WorkTree::Ancestor);
        assert_eq!(found.at, Some(above.display().to_string()));
    }

    #[tokio::test]
    async fn ensuring_an_unversioned_folder_initialises_it_once() {
        let (_dir, root) = folder();
        if !has_git(&root) {
            return;
        }

        let first = ensure(&root, None).await;

        assert!(first.initialized, "{first:?}");
        assert_eq!(first.problem, None);
        assert_eq!(first.versioning.tree, WorkTree::Here);
        assert!(root.join(GIT_ENTRY).exists());

        // And the act of creating the repository commits nothing: a fresh
        // `git init` has no branch at all until something is committed, which
        // is exactly the state PLAN 7.11 asks to be left in.
        let heads = root.join(".git").join("refs").join("heads");
        assert_eq!(
            fs::read_dir(&heads).map(|read| read.count()).unwrap_or(0),
            0,
            "nothing was committed"
        );

        let second = ensure(&root, None).await;
        assert!(!second.initialized, "the second run leaves it alone");
        assert_eq!(second.versioning.tree, WorkTree::Here);
    }

    /// The never that matters most: an inner `.git` splits a history and hides
    /// the outer one.
    #[tokio::test]
    async fn ensuring_a_folder_inside_a_repository_never_nests_one() {
        let (dir, root) = folder();
        let above = dunce::canonicalize(dir.path()).expect("canonical");
        fs::create_dir(above.join(GIT_ENTRY)).expect("a repository above");

        let ensured = ensure(&root, None).await;

        assert!(!ensured.initialized);
        assert_eq!(ensured.versioning.tree, WorkTree::Ancestor);
        assert!(!root.join(GIT_ENTRY).exists(), "no nested repository");
    }

    /// Nothing about the folder's contents is git's business here: a
    /// repository is created over whatever is already in it, and none of it is
    /// added, staged or committed.
    #[tokio::test]
    async fn initialising_leaves_the_files_that_were_already_there() {
        let (_dir, root) = folder();
        if !has_git(&root) {
            return;
        }
        fs::write(root.join("notes.md"), "ours\n").expect("write");

        assert!(ensure(&root, None).await.initialized);

        assert_eq!(
            fs::read_to_string(root.join("notes.md")).expect("read"),
            "ours\n"
        );
        assert!(
            !root.join(".gitignore").exists(),
            "no .gitignore was invented"
        );
    }

    /// The third row of PLAN 7.11's table, on a machine that can show it: the
    /// folder stays unversioned and the report says why.
    /// PLAN 7.12: off Windows, a named WSL host is refused, never swapped for
    /// the local `git`.
    #[tokio::test]
    #[cfg(not(windows))]
    async fn a_wsl_host_off_windows_refuses_rather_than_falling_back() {
        let (_dir, root) = folder();
        let host = ExecHost::Wsl {
            distro: "Ubuntu".to_owned(),
        };

        let ensured = ensure(&root, Some(&host)).await;

        assert!(!ensured.initialized);
        assert!(
            !root.join(GIT_ENTRY).exists(),
            "nothing was initialised here"
        );
        assert!(
            ensured.problem.is_some_and(|why| why.contains("Ubuntu")),
            "the reason names the distribution"
        );
    }

    /// A distribution nobody is registered under is a reported problem, not a
    /// repository made by the wrong `git`. The folder is left alone.
    #[tokio::test]
    #[cfg(windows)]
    async fn an_unknown_distribution_leaves_the_folder_alone() {
        let (_dir, root) = folder();
        let host = ExecHost::Wsl {
            distro: "aegis-no-such-distro".to_owned(),
        };

        let ensured = ensure(&root, Some(&host)).await;

        assert!(!ensured.initialized, "{ensured:?}");
        assert_eq!(ensured.versioning.tree, WorkTree::Unversioned);
        assert!(
            !root.join(GIT_ENTRY).exists(),
            "and this computer's git did not step in"
        );
        assert!(
            ensured
                .problem
                .is_some_and(|why| why.contains("aegis-no-such-distro")),
            "the reason names the distribution"
        );
    }

    #[tokio::test]
    async fn a_missing_git_is_a_reported_problem() {
        let (_dir, root) = folder();
        if has_git(&root) {
            return;
        }

        let ensured = ensure(&root, None).await;

        assert!(!ensured.initialized);
        assert_eq!(ensured.versioning.tree, WorkTree::Unversioned);
        assert!(
            ensured.problem.is_some_and(|why| why.contains(GIT)),
            "the reason names the program"
        );
    }
}
