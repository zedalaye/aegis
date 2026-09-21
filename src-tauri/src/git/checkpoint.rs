//! Run checkpoints (PLAN 7.24).
//!
//! Before and after an unattended or delegated run that may write, the working
//! tree is recorded on a side ref, `refs/aegis/runs/<run>/{before,after}`,
//! where `<run>` is the run's session. The harness snapshots; the agent never
//! commits.
//!
//! * **Nothing of the operator's moves**: the snapshot is built in a temporary
//!   index (`GIT_INDEX_FILE`, seeded from a copy of the real one so its stat
//!   cache spares a rehash), then `write-tree`, `commit-tree` and
//!   `update-ref`. `HEAD`, the index, branches and the stash are untouched.
//! * **Not a commit on a branch**: the author is set in the child's
//!   environment only, the default refspec does not carry `refs/aegis/`, and
//!   no hook runs. Ignored files are not captured.
//! * **Scoped to the workspace**: `add -A -- .` from the workspace root, so a
//!   workspace inside a larger repository snapshots its own folder over the
//!   rest of the repository's index.
//! * **Restore is the operator's**: the paths the run changed go back to the
//!   `before` tree, except a path changed again since the run ended, which is
//!   somebody else's work and is left alone.
//!
//! On the project's execution host (PLAN 7.12), like [`ensure`](super::ensure).

use std::collections::{BTreeMap, HashSet};
use std::path::{Component, Path};
use std::process::Stdio;
use std::time::Duration;

use serde::Serialize;
use tokio::io::AsyncWriteExt as _;
use tokio::process::Command;
use ts_rs::TS;

use crate::exec_host::{self, ExecHost};

/// Where every run's refs live.
pub const REF_ROOT: &str = "refs/aegis/runs";

/// Checkpoints older than this many days are pruned.
pub const KEEP_DAYS: i64 = 30;

/// Most runs kept per work tree, newest first.
pub const KEEP_RUNS: usize = 200;

/// Longest patch the board is sent; the file list is always whole.
const PATCH_MAX_BYTES: usize = 128 * 1024;

/// How long one `git` may take. `add -A` over a large tree is the slow one.
const TIMEOUT: Duration = Duration::from_secs(120);

/// Who the checkpoint commits say made them, set on the child only.
const AUTHOR: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "Aegis"),
    ("GIT_AUTHOR_EMAIL", "aegis@localhost"),
    ("GIT_COMMITTER_NAME", "Aegis"),
    ("GIT_COMMITTER_EMAIL", "aegis@localhost"),
];

// ---------------------------------------------------------------------------
// IPC payloads
// ---------------------------------------------------------------------------

/// How one path differs between a run's two trees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum ChangeKind {
    /// The run created it.
    Added,
    /// The run changed its content or mode.
    Modified,
    /// The run removed it.
    Deleted,
}

/// One path the run changed, relative to the workspace root, `/`-separated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct Change {
    /// The path.
    pub path: String,
    /// What the run did to it.
    pub kind: ChangeKind,
}

/// A run's checkpoint, as the board shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts", rename = "RunCheckpoint")]
pub struct Checkpoint {
    /// The run: its session.
    pub run_id: String,
    /// When the `before` tree was taken, RFC3339.
    pub before_at: String,
    /// When the `after` tree was taken, RFC3339; empty while the run has not
    /// ended, or if it never did.
    pub after_at: String,
    /// What the run changed, by path. Empty without an `after`.
    pub changes: Vec<Change>,
    /// The unified diff, cut at [`PATCH_MAX_BYTES`].
    pub patch: String,
    /// Whether [`Checkpoint::patch`] was cut.
    pub truncated: bool,
}

/// What a restore did, path by path.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts", rename = "RunRestored")]
pub struct Restored {
    /// Paths written back from the `before` tree.
    pub restored: Vec<String>,
    /// Paths the run created, now removed.
    pub removed: Vec<String>,
    /// Paths changed again since the run ended, left as they are.
    pub kept: Vec<String>,
}

/// Which end of a run a snapshot is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// Taken as the run opens; never replaced, so a resumed or retried run
    /// keeps the tree from before any of it.
    Before,
    /// Taken as the run ends; replaced each time it ends again.
    After,
}

impl Side {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Before => "before",
            Self::After => "after",
        }
    }
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// Whether an identity holding these tools may write the tree, so its runs are
/// worth a checkpoint.
pub fn may_write(tools: &[String]) -> bool {
    tools.iter().any(|tool| {
        tool == crate::policy::tool::FS_WRITE || tool == crate::policy::tool::SHELL_EXEC
    })
}

/// Records one end of a run, and prunes after an `after`. Never fails: a
/// workspace that is not a work tree has no checkpoint, and a `git` that
/// refuses is logged. The run goes on either way.
pub async fn mark(root: &Path, host: Option<&ExecHost>, run_id: &str, side: Side) {
    if !super::measure(root).tree.is_versioned() {
        return;
    }
    match record(root, host, run_id, side).await {
        Ok(commit) => tracing::debug!(run_id, side = side.as_str(), %commit, "checkpoint taken"),
        Err(problem) => {
            tracing::warn!(run_id, side = side.as_str(), %problem, "no checkpoint was taken");
            return;
        }
    }
    if side == Side::After {
        let now = chrono::Utc::now().timestamp();
        if let Err(problem) = Git::new(root, host).prune(now, KEEP_RUNS, KEEP_DAYS).await {
            tracing::warn!(%problem, "old checkpoints could not be pruned");
        }
    }
}

/// Takes one end of a run and points its ref at it. A `before` that already
/// exists is kept, and its commit returned.
pub async fn record(
    root: &Path,
    host: Option<&ExecHost>,
    run_id: &str,
    side: Side,
) -> Result<String, String> {
    let name = ref_name(run_id, side)?;
    let git = Git::new(root, host);

    if side == Side::Before {
        if let Some(existing) = git.resolve(&name).await? {
            return Ok(existing);
        }
    }

    let parent = git.resolve("HEAD^{commit}").await?;
    let tree = git.snapshot(parent.as_deref()).await?;
    let message = format!("aegis: run {run_id}, {}", side.as_str());
    let mut args = vec!["commit-tree", "--no-gpg-sign", tree.as_str()];
    if let Some(parent) = parent.as_deref() {
        args.extend(["-p", parent]);
    }
    args.extend(["-m", message.as_str()]);
    let commit = git.text(&args, &AUTHOR, None).await?;

    git.text(
        &["update-ref", "-m", "aegis: checkpoint", &name, &commit],
        &[],
        None,
    )
    .await?;
    Ok(commit)
}

/// A run's checkpoint, or `None` when it has none: not a work tree, or a run
/// that was never checkpointed (or has been pruned).
pub async fn read(
    root: &Path,
    host: Option<&ExecHost>,
    run_id: &str,
) -> Result<Option<Checkpoint>, String> {
    if !super::measure(root).tree.is_versioned() {
        return Ok(None);
    }
    let git = Git::new(root, host);
    let ends = git.ends(run_id).await?;
    let Some(before) = ends.before else {
        return Ok(None);
    };

    let mut checkpoint = Checkpoint {
        run_id: run_id.to_owned(),
        before_at: before.at,
        after_at: String::new(),
        changes: Vec::new(),
        patch: String::new(),
        truncated: false,
    };
    let Some(after) = ends.after else {
        return Ok(Some(checkpoint));
    };

    checkpoint.after_at = after.at;
    checkpoint.changes = git.changes(&before.commit, &after.commit).await?;
    let patch = git
        .bytes(
            &[
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--no-renames",
                "--relative",
                &before.commit,
                &after.commit,
            ],
            &[],
            None,
        )
        .await?;
    checkpoint.truncated = patch.len() > PATCH_MAX_BYTES;
    checkpoint.patch = String::from_utf8_lossy(&patch[..patch.len().min(PATCH_MAX_BYTES)])
        .trim_end_matches('\u{FFFD}')
        .to_owned();
    Ok(Some(checkpoint))
}

/// Puts back the `before` tree of every path the run changed, except the ones
/// changed again since it ended.
pub async fn restore(
    root: &Path,
    host: Option<&ExecHost>,
    run_id: &str,
) -> Result<Restored, String> {
    if !super::measure(root).tree.is_versioned() {
        return Err("this workspace is not a git work tree, so no run was checkpointed".to_owned());
    }
    let git = Git::new(root, host);
    let ends = git.ends(run_id).await?;
    let (Some(before), Some(after)) = (ends.before, ends.after) else {
        return Err(
            "this run has no finished checkpoint: it is still running, or it never ended"
                .to_owned(),
        );
    };

    let changes = git.changes(&before.commit, &after.commit).await?;
    // The tree as it is now, against the run's `after`: whatever moved since
    // is not the run's to take back.
    let parent = git.resolve("HEAD^{commit}").await?;
    let now = git.snapshot(parent.as_deref()).await?;
    let moved: HashSet<String> = git
        .changes(&after.commit, &now)
        .await?
        .into_iter()
        .map(|change| change.path)
        .collect();

    let mut done = Restored::default();
    let mut back = Vec::new();
    for change in changes {
        if moved.contains(&change.path) {
            done.kept.push(change.path);
        } else if change.kind == ChangeKind::Added {
            remove(root, &change.path)?;
            done.removed.push(change.path);
        } else {
            back.push(change.path);
        }
    }

    if !back.is_empty() {
        // Worktree only: `restore` without `--staged` leaves the index alone.
        // `:(literal)` so a path with `*` in it names that path.
        let mut list = Vec::new();
        for path in &back {
            list.extend_from_slice(b":(literal)");
            list.extend_from_slice(path.as_bytes());
            list.push(0);
        }
        let source = format!("--source={}", before.commit);
        git.text(
            &[
                "restore",
                &source,
                "--worktree",
                "--pathspec-from-file=-",
                "--pathspec-file-nul",
            ],
            &[],
            Some(list),
        )
        .await?;
        done.restored = back;
    }

    tracing::info!(
        run_id,
        restored = done.restored.len(),
        removed = done.removed.len(),
        kept = done.kept.len(),
        "a run's checkpoint was restored"
    );
    Ok(done)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The ref one end of a run lives on.
fn ref_name(run_id: &str, side: Side) -> Result<String, String> {
    Ok(format!("{}/{}", run_ref(run_id)?, side.as_str()))
}

/// The folder of refs one run lives under. A run id is a session id:
/// letters, digits, `-` and `_`, so it is one safe ref component.
fn run_ref(run_id: &str) -> Result<String, String> {
    let fine = !run_id.is_empty()
        && run_id.len() <= 64
        && run_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_');
    if !fine {
        return Err(format!("`{run_id}` is not a run id"));
    }
    Ok(format!("{REF_ROOT}/{run_id}"))
}

/// Removes a file the run created, and the folders that held only it.
fn remove(root: &Path, rel: &str) -> Result<(), String> {
    let rel = Path::new(rel);
    if !rel
        .components()
        .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err(format!(
            "`{}` is not a path inside the workspace",
            rel.display()
        ));
    }
    let path = root.join(rel);
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(format!("`{}` could not be removed ({err})", rel.display())),
    }
    for dir in path.ancestors().skip(1) {
        if dir == root || !dir.starts_with(root) || std::fs::remove_dir(dir).is_err() {
            break;
        }
    }
    Ok(())
}

/// One ref of a run: its commit, and when it was taken.
struct End {
    commit: String,
    at: String,
}

/// A run's two ends, as far as they exist.
#[derive(Default)]
struct Ends {
    before: Option<End>,
    after: Option<End>,
}

/// `git`, in one workspace, on its host.
struct Git<'a> {
    root: &'a Path,
    host: Option<&'a ExecHost>,
}

impl<'a> Git<'a> {
    const fn new(root: &'a Path, host: Option<&'a ExecHost>) -> Self {
        Self { root, host }
    }

    /// The tree of the workspace as it is on disk, over the rest of the
    /// repository's index.
    async fn snapshot(&self, parent: Option<&str>) -> Result<String, String> {
        let gitdir = self
            .text(&["rev-parse", "--absolute-git-dir"], &[], None)
            .await?;
        let index = format!("{gitdir}/aegis-checkpoint-{}.index", uuid::Uuid::new_v4());
        let tree = self.snapshot_into(&gitdir, &index, parent).await;
        self.discard(&index).await;
        tree
    }

    async fn snapshot_into(
        &self,
        gitdir: &str,
        index: &str,
        parent: Option<&str>,
    ) -> Result<String, String> {
        let env = [("GIT_INDEX_FILE", index)];
        // The operator's index is copied, never opened for writing: its stat
        // cache is what spares `add` a rehash of every file.
        if !self.copy(&format!("{gitdir}/index"), index).await {
            if let Some(parent) = parent {
                self.text(&["read-tree", parent], &env, None).await?;
            }
        }
        self.text(&["add", "-A", "--", "."], &env, None).await?;
        self.text(&["write-tree"], &env, None).await
    }

    /// The commit a name resolves to, or `None` when it does not.
    async fn resolve(&self, name: &str) -> Result<Option<String>, String> {
        let ran = self
            .run(
                Program::Git,
                &["rev-parse", "--verify", "--quiet", name],
                &[],
                None,
            )
            .await?;
        match ran.status.code() {
            Some(0) => Ok(Some(String::from_utf8_lossy(&ran.stdout).trim().to_owned())),
            Some(1) => Ok(None),
            _ => Err(self.failure("rev-parse", &ran)),
        }
    }

    /// Both ends of a run, read in one call.
    async fn ends(&self, run_id: &str) -> Result<Ends, String> {
        let prefix = format!("{}/", run_ref(run_id)?);
        let listed = self
            .text(
                &[
                    "for-each-ref",
                    "--format=%(refname)%00%(objectname)%00%(committerdate:iso-strict)",
                    &prefix,
                ],
                &[],
                None,
            )
            .await?;

        let mut ends = Ends::default();
        for line in listed.lines() {
            let mut fields = line.split('\0');
            let (Some(name), Some(commit), Some(at)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let end = Some(End {
                commit: commit.to_owned(),
                at: at.to_owned(),
            });
            match name.strip_prefix(&prefix) {
                Some("before") => ends.before = end,
                Some("after") => ends.after = end,
                _ => {}
            }
        }
        Ok(ends)
    }

    /// What differs between two tree-ishes, under the workspace.
    async fn changes(&self, from: &str, to: &str) -> Result<Vec<Change>, String> {
        let out = self
            .bytes(
                &[
                    "diff",
                    "--name-status",
                    "--no-renames",
                    "--relative",
                    "-z",
                    from,
                    to,
                ],
                &[],
                None,
            )
            .await?;
        let mut fields = out
            .split(|byte| *byte == 0)
            .filter(|field| !field.is_empty());
        let mut changes = Vec::new();
        while let (Some(status), Some(path)) = (fields.next(), fields.next()) {
            let kind = match status.first() {
                Some(b'A') => ChangeKind::Added,
                Some(b'D') => ChangeKind::Deleted,
                _ => ChangeKind::Modified,
            };
            changes.push(Change {
                path: String::from_utf8_lossy(path).into_owned(),
                kind,
            });
        }
        Ok(changes)
    }

    /// Drops the checkpoints older than `keep_days` and beyond the newest
    /// `keep_runs` runs. Returns how many runs went.
    async fn prune(&self, now: i64, keep_runs: usize, keep_days: i64) -> Result<usize, String> {
        let listed = self
            .text(
                &[
                    "for-each-ref",
                    "--format=%(refname)%00%(committerdate:unix)",
                    &format!("{REF_ROOT}/"),
                ],
                &[],
                None,
            )
            .await?;

        let mut runs: BTreeMap<String, (i64, Vec<String>)> = BTreeMap::new();
        for line in listed.lines() {
            let mut fields = line.split('\0');
            let (Some(name), Some(at)) = (fields.next(), fields.next()) else {
                continue;
            };
            let Some(run) = name
                .strip_prefix(REF_ROOT)
                .and_then(|rest| rest.trim_start_matches('/').split('/').next())
            else {
                continue;
            };
            let at = at.trim().parse::<i64>().unwrap_or(0);
            let entry = runs.entry(run.to_owned()).or_insert((at, Vec::new()));
            entry.0 = entry.0.max(at);
            entry.1.push(name.to_owned());
        }

        let mut newest: Vec<(i64, Vec<String>)> = runs.into_values().collect();
        newest.sort_by_key(|run| std::cmp::Reverse(run.0));
        let oldest = now - keep_days * 24 * 60 * 60;

        let mut script = String::new();
        let mut pruned = 0;
        for (rank, (at, names)) in newest.iter().enumerate() {
            if rank < keep_runs && *at >= oldest {
                continue;
            }
            pruned += 1;
            for name in names {
                script.push_str(&format!("delete {name}\n"));
            }
        }
        if pruned > 0 {
            self.text(&["update-ref", "--stdin"], &[], Some(script.into_bytes()))
                .await?;
            tracing::info!(pruned, "old checkpoints pruned");
        }
        Ok(pruned)
    }

    /// Copies a file on the host. `false` when it could not, which for the
    /// index means a repository that has never staged anything.
    async fn copy(&self, from: &str, to: &str) -> bool {
        match self.host {
            None => std::fs::copy(from, to).is_ok(),
            Some(_) => self
                .run(Program::Named("cp"), &["--", from, to], &[], None)
                .await
                .is_ok_and(|ran| ran.status.success()),
        }
    }

    /// Removes the temporary index, best effort.
    async fn discard(&self, index: &str) {
        let gone = match self.host {
            None => std::fs::remove_file(index).is_ok(),
            Some(_) => self
                .run(Program::Named("rm"), &["-f", "--", index], &[], None)
                .await
                .is_ok_and(|ran| ran.status.success()),
        };
        if !gone {
            tracing::warn!(index, "a checkpoint's temporary index was left behind");
        }
    }

    /// A `git` that has to succeed, its output trimmed.
    async fn text(
        &self,
        args: &[&str],
        env: &[(&str, &str)],
        stdin: Option<Vec<u8>>,
    ) -> Result<String, String> {
        let out = self.bytes(args, env, stdin).await?;
        Ok(String::from_utf8_lossy(&out).trim().to_owned())
    }

    /// A `git` that has to succeed, its output as it came.
    async fn bytes(
        &self,
        args: &[&str],
        env: &[(&str, &str)],
        stdin: Option<Vec<u8>>,
    ) -> Result<Vec<u8>, String> {
        let ran = self.run(Program::Git, args, env, stdin).await?;
        if ran.status.success() {
            Ok(ran.stdout)
        } else {
            Err(self.failure(args.first().copied().unwrap_or("git"), &ran))
        }
    }

    /// Why a `git` came back unhappy, in one line.
    fn failure(&self, verb: &str, ran: &std::process::Output) -> String {
        let said = exec_host::message(&ran.stderr);
        if said.is_empty() {
            format!("`git {verb}` exited with {:?}", ran.status.code())
        } else {
            format!("`git {verb}` said: {said}")
        }
    }

    /// Spawns one program on the host, in the workspace.
    async fn run(
        &self,
        program: Program,
        args: &[&str],
        env: &[(&str, &str)],
        stdin: Option<Vec<u8>>,
    ) -> Result<std::process::Output, String> {
        let mut command = self.command(program, args, env)?;
        command
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A `git` that decides to ask something must not wait on a
            // terminal nobody is attached to.
            .env("GIT_TERMINAL_PROMPT", "0")
            .kill_on_drop(true);
        super::no_window(&mut command);

        let work = async {
            let mut child = command
                .spawn()
                .map_err(|err| format!("`git` would not start ({err})"))?;
            if let (Some(bytes), Some(mut pipe)) = (stdin, child.stdin.take()) {
                pipe.write_all(&bytes)
                    .await
                    .map_err(|err| format!("`git` would not read its input ({err})"))?;
            }
            child
                .wait_with_output()
                .await
                .map_err(|err| format!("`git` did not finish ({err})"))
        };
        tokio::time::timeout(TIMEOUT, work)
            .await
            .map_err(|_| format!("`git` did not answer within {} seconds", TIMEOUT.as_secs()))?
    }

    /// The command for one program: this computer's `git` in the workspace,
    /// or the distribution's through `wsl.exe --exec` (PLAN 7.12).
    fn command(
        &self,
        program: Program,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> Result<Command, String> {
        match self.host {
            None => {
                let mut command = Command::new(super::program(self.root)?);
                command.args(args).current_dir(self.root);
                command.envs(env.iter().copied());
                Ok(command)
            }
            Some(ExecHost::Wsl { distro }) => wsl(distro, self.root, program, args, env),
        }
    }
}

/// Which program a [`Git::run`] spawns. Only a WSL host ever needs another:
/// on this computer, files are copied and removed from Rust.
#[derive(Clone, Copy)]
enum Program {
    Git,
    Named(&'static str),
}

/// A program in a WSL distribution. `git -C` rather than `wsl.exe --cd`,
/// which silently starts in `~` for a folder that is not there — and a
/// snapshot of the wrong repository is worse than none.
#[cfg(windows)]
fn wsl(
    distro: &str,
    root: &Path,
    program: Program,
    args: &[&str],
    env: &[(&str, &str)],
) -> Result<Command, String> {
    let exe = exec_host::wsl_exe().ok_or_else(|| {
        format!(
            "this project runs its commands in `{distro}`, and `wsl.exe` is not on this machine"
        )
    })?;
    let cwd = exec_host::linux_path(distro, root)?;

    let mut command = Command::new(exe);
    command.args(["-d", distro, "--exec", "env"]);
    for (key, value) in env {
        command.arg(format!("{key}={value}"));
    }
    match program {
        Program::Git => {
            command.args([super::GIT, "-C", &cwd]);
        }
        Program::Named(name) => {
            command.arg(name);
        }
    }
    command.args(args);
    Ok(command)
}

/// A program in a WSL distribution. Never, off Windows.
#[cfg(not(windows))]
fn wsl(
    distro: &str,
    _root: &Path,
    _program: Program,
    _args: &[&str],
    _env: &[(&str, &str)],
) -> Result<Command, String> {
    Err(format!(
        "this project runs its commands in `{distro}`, and this computer has no WSL"
    ))
}

#[cfg(test)]
mod tests;
