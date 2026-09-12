//! Workspace versioning (PLAN 7.11).
//!
//! The missed half of Phase 11. Shared memory is files, and `.aegis/status/STATUS.md`
//! is a board rewritten in place: without history, yesterday is gone and the
//! transcript is once again the only log — the one thing the convention exists
//! to stop being. Phase 11 laid down the directories and never created the
//! repository.
//!
//! So a workspace whose convention is laid down is a git work tree. That is a
//! fact about *versioning the files*, not about being a software project
//! (PLAN 7.1, *Project*): a finance folder and a watch folder get a repository
//! on the same terms as a repo full of Rust.
//!
//! Two operations, and the asymmetry between them is the whole design.
//!
//! * [`measure`] is a read, and it never runs a program. It walks for a `.git`
//!   from the workspace root upwards, which is what makes it cheap enough to
//!   sit in [`workspace::layout`](crate::workspace::layout) — measured on every
//!   panel render and after every turn — and what makes it truthful on a
//!   machine with no `git` at all.
//! * [`ensure`] is a write, it runs exactly once, and only from
//!   [`workspace_scaffold`](crate::commands::workspace::workspace_scaffold):
//!   the button is the consent. Picking a folder is not consent to mutate it,
//!   so `project_create` never comes here.
//!
//! ## Whose `git`
//!
//! The project's, not this computer's (PLAN 7.11, 7.12). A Windows operator
//! whose repository lives in a WSL distribution has two `git`s on the machine,
//! and the Windows one on a `\\wsl$\` tree is the wrong one: it writes a
//! `.git` the distribution's toolchain then reads across the 9p boundary, with
//! that distro's `core.autocrlf`, `safe.directory` and hooks all absent from
//! the reckoning. So when the project names an execution host, `git init` goes
//! through `wsl.exe` exactly as `shell_exec` does — same `--exec`, same
//! `--cd`, same `test -d` probe first, because `wsl.exe --cd` answers a folder
//! it cannot find by starting in the user's home and saying nothing. A
//! repository created in somebody's Linux home instead of their project is the
//! one failure here that looks like success.
//!
//! [`measure`] needs none of that: `fs_*` already sees a distribution's files
//! over the UNC path, and so does a walk for `.git`.
//!
//! ## What this is not
//!
//! Not a git client. There is no log, no stage, no push, no diff, and no
//! Commit button anywhere in the WebView — a privileged commit from the runtime
//! would be a second write path around the dialog the agent already has. A
//! commit is `shell_exec` of `git`, under the gate, asked for by a person:
//! either in their own terminal, or in the session, where `allow_session` keys
//! on the basename `git` and still shows the exact line of every later
//! `git push` or `git reset`.
//!
//! And nothing here writes a remote, a `.gitignore`, a `user.name`, a
//! `user.email` or a commit. An `fs_write` approved by a human is approval to
//! write a file; it is not approval to put it on a branch. The first
//! `git commit` in a folder needs a git identity, and if the machine has none
//! the error belongs in the approval dialog where the person can fix it — not
//! in an email this runtime invented for them.
//!
//! ## Never a nested repository
//!
//! An inner `.git` splits a history and hides the outer one, and it is the easy
//! mistake here: `git init` in a subdirectory of a repository succeeds without
//! complaint. [`ensure`] therefore measures before it acts, and a workspace
//! that is a folder *inside* somebody's monorepo is reported as versioned by
//! that ancestor and left exactly as it is.

use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use serde::Serialize;
use tokio::process::Command;
use ts_rs::TS;

use crate::exec_host::{self, ExecHost};
use crate::tools::shell;

/// The entry that marks the top of a work tree.
///
/// A directory in an ordinary clone and a *file* in a linked worktree or a
/// submodule, which is why everything here asks whether it exists rather than
/// whether it is a directory.
const GIT_ENTRY: &str = ".git";

/// The program, by the name PATH knows it under.
const GIT: &str = "git";

/// How long a distribution has to answer before the folder is left unversioned.
///
/// The same budget `shell_exec`'s probe allows, and for the same reason: a WSL
/// virtual machine that is not running yet takes seconds to come up the first
/// time, and a distribution that will never answer must not hold the button
/// down forever.
const HOST_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// IPC payloads
// ---------------------------------------------------------------------------

/// Whether the folder is in a work tree, and whose.
///
/// Three answers rather than a boolean because the middle one changes what the
/// panel should say and what [`ensure`] must not do: a workspace inside a
/// larger repository is already versioned, and initialising it would split the
/// history it is already part of.
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
    /// The folder holding the `.git`, absolute — `None` when there is none.
    ///
    /// Named rather than left implicit because the answer a person needs from
    /// [`WorkTree::Ancestor`] is *which* folder: "this is inside a repository"
    /// is only useful once you can see whether it is the one you meant.
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

/// Where the `.git` for this folder is, if anywhere.
///
/// A filesystem walk rather than `git rev-parse`, deliberately. This runs
/// wherever [`workspace::layout`](crate::workspace::layout) runs — every render
/// of the rail, after every turn — and spawning a process on that path would be
/// paying for a subprocess to answer a question a `stat` answers. It also gives
/// the honest answer on a machine where `git` is not installed, which is the
/// one case where an answer from `git` is unavailable and the question is still
/// worth asking.
///
/// The one thing the walk does not copy from git is `GIT_CEILING_DIRECTORIES`
/// and the refusal to cross a filesystem boundary. A workspace whose ancestor
/// repository is across a mount point is reported here as versioned by it; git
/// would disagree, [`ensure`] would decline to init, and the report names the
/// ancestor — so the miss is visible rather than silent.
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

/// What one scaffolding run did about versioning.
///
/// Not an IPC payload of its own: the three fields are spread onto
/// [`ScaffoldReport`](crate::workspace::ScaffoldReport), because what the panel
/// has to say is one sentence and a nested object would only make the UI walk
/// further to write it.
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
/// Three outcomes, and only the middle one touches the disk:
///
/// * already a work tree, here or above — left exactly as it is;
/// * not one — `git init` here, and nothing else. No commit, no remote, no
///   `.gitignore`, no identity;
/// * `git` unreachable — the folder stays unversioned and the reason is
///   carried back for the report.
///
/// `host` is the project's, and it decides *whose* `git` runs: `None` is this
/// computer, `Some` is the distribution the operator named, reached exactly the
/// way `shell_exec` reaches it. It is never inferred from the path — a folder
/// under `\\wsl$\` is not consent, the same rule PLAN 7.12 sets for commands.
///
/// Never fails. A scaffolding run that laid down five directories and could not
/// reach a `git` has done the thing it was pressed for; turning that into an
/// error would throw away the part that worked to report the part that did not.
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
/// Spawned by the runtime rather than reached through the agent's `shell_exec`:
/// the person pressed a button, and routing their own button through the
/// approval dialog would be asking them to confirm what they just asked for.
/// The reverse — a *commit* from the runtime — is the one this never becomes,
/// for the symmetrical reason (see the module header).
///
/// Bare `git init`, with no arguments at all. `--initial-branch` would be this
/// runtime picking a branch name for somebody's repository, and a template or
/// a `.gitignore` would be it picking their content; whatever their
/// `init.defaultBranch` and `init.templateDir` say is what they get, because
/// that is what they would have got in a terminal.
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
/// `--exec` rather than a command line, so there is no login shell between
/// `wsl.exe` and `git` and no metacharacter layer — the same shape `shell_exec`
/// uses, for the same reason.
///
/// The probe before it is not optional and is why this is `async`. `wsl.exe
/// --cd` answers a directory it cannot find by starting in the user's home and
/// saying nothing, so without `test -d` a folder the distribution has no path
/// for would be answered with a repository in somebody's `~`. That is the one
/// failure here that would look like success.
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
/// Not unreachable: a `projects.json` written on Windows and opened on another
/// machine carries the host with it. Refusing is the point — the alternative is
/// initialising with this computer's `git` instead, which is the wrong
/// toolchain and would look exactly like success.
#[cfg(not(windows))]
async fn inside(distro: &str, _root: &Path) -> Result<Command, String> {
    Err(format!(
        "this project runs its commands in `{distro}`, and this computer has no WSL"
    ))
}

/// Waits for a child, or says what stopped answering.
///
/// A distribution that will never come up must not hold the button down for
/// ever, and the folder it could not version is a line in the report like any
/// other reason.
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
/// PATH first, through the resolver `shell_exec` and the MCP client already
/// share, so "which git" cannot mean two things in one process. Then the
/// handful of places it is installed, because a GUI-launched app does not
/// inherit the PATH a login shell builds — the same class of problem PLAN 7.7
/// records for `keybase.exe` — and "git is not installed" is the wrong thing to
/// tell somebody who has been using it in that folder all morning.
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

    /// Whether this machine can run the half of these tests that needs git.
    ///
    /// Written as a skip rather than as an assertion, for the reason the
    /// screen-capture tests are written the way they are: a developer's machine
    /// has `git` and a minimal container may not, and what is worth testing is
    /// what happens in each case — not that the suite only passes on one of
    /// them. Both branches are covered: the tests below that need git return
    /// early without it, and [`a_missing_git_is_a_reported_problem`] is the one
    /// that only runs when it is absent.
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
    /// PLAN 7.12: a host the project names is never silently swapped for this
    /// computer. Off Windows there is no WSL to reach, and initialising with
    /// the local `git` instead would be the wrong toolchain looking exactly
    /// like success.
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
