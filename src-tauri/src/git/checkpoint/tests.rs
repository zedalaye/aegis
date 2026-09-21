//! Checkpoints against a real `git`; each test returns early without one.

use super::*;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

const RUN: &str = "0b7e6a40-1c1f-4c55-9c1d-3a2b4c5d6e7f";

/// A canonical, empty folder.
fn folder() -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("temp dir");
    let root = dunce::canonicalize(dir.path()).expect("canonical");
    (dir, root)
}

/// Runs the operator's `git` in `root`, as a person at a terminal would.
fn git(root: &Path, args: &[&str]) -> String {
    let program = super::super::program(root).expect("git");
    let out = std::process::Command::new(program)
        .args([
            "-c",
            "user.name=Operator",
            "-c",
            "user.email=op@example.com",
        ])
        .args(["-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"])
        .args(args)
        .current_dir(root)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A repository with one commit of two files, or `None` without `git`.
fn repository() -> Option<(TempDir, PathBuf)> {
    let (dir, root) = folder();
    super::super::program(&root).ok()?;
    git(&root, &["init", "-q"]);
    git(&root, &["config", "core.autocrlf", "false"]);
    fs::write(root.join("keep.md"), "kept\n").expect("write");
    fs::write(root.join("gone.md"), "about to go\n").expect("write");
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "start"]);
    Some((dir, root))
}

/// What a person would check the run did not touch.
fn operator_view(root: &Path) -> (String, String) {
    (
        git(root, &["status", "--porcelain=v1", "-uall"]),
        git(root, &["log", "--branches", "--format=%H %s"]),
    )
}

/// The operator's index, byte for byte. Read before any `git status`, which
/// refreshes it on its own.
fn index(root: &Path) -> Vec<u8> {
    fs::read(root.join(".git").join("index")).expect("index")
}

/// PLAN 7.24's exit: three files written, a diff on the board, a restore that
/// returns them, and the operator's status and log unchanged.
#[tokio::test]
async fn a_run_that_writes_three_files_is_shown_and_taken_back() {
    let Some((_dir, root)) = repository() else {
        return;
    };
    // Work of the operator's own, staged and not, that the run must not eat.
    fs::write(root.join("draft.md"), "mine\n").expect("write");
    let seen = operator_view(&root);
    let staged = index(&root);

    record(&root, None, RUN, Side::Before)
        .await
        .expect("before");
    fs::write(root.join("keep.md"), "rewritten by the run\n").expect("write");
    fs::remove_file(root.join("gone.md")).expect("remove");
    fs::create_dir_all(root.join("out").join("deep")).expect("mkdir");
    fs::write(root.join("out").join("deep").join("new.md"), "new\n").expect("write");
    record(&root, None, RUN, Side::After).await.expect("after");

    let checkpoint = read(&root, None, RUN).await.expect("read").expect("some");
    let mut changes = checkpoint.changes.clone();
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    assert_eq!(
        changes,
        vec![
            Change {
                path: "gone.md".to_owned(),
                kind: ChangeKind::Deleted
            },
            Change {
                path: "keep.md".to_owned(),
                kind: ChangeKind::Modified
            },
            Change {
                path: "out/deep/new.md".to_owned(),
                kind: ChangeKind::Added
            },
        ]
    );
    assert!(checkpoint.patch.contains("rewritten by the run"));
    assert!(!checkpoint.after_at.is_empty());
    assert!(!checkpoint.truncated);

    let done = restore(&root, None, RUN).await.expect("restore");
    assert_eq!(index(&root), staged, "the index was never written");
    assert_eq!(done.removed, vec!["out/deep/new.md".to_owned()]);
    assert_eq!(done.restored.len(), 2);
    assert!(done.kept.is_empty());

    assert_eq!(
        fs::read_to_string(root.join("keep.md")).expect("read"),
        "kept\n"
    );
    assert_eq!(
        fs::read_to_string(root.join("gone.md")).expect("read"),
        "about to go\n"
    );
    assert!(!root.join("out").exists(), "the folders it made went too");
    assert_eq!(operator_view(&root), seen, "status and log unchanged");
}

/// A path somebody changed after the run is theirs now.
#[tokio::test]
async fn a_path_changed_since_the_run_is_left_alone() {
    let Some((_dir, root)) = repository() else {
        return;
    };
    record(&root, None, RUN, Side::Before)
        .await
        .expect("before");
    fs::write(root.join("keep.md"), "the run's\n").expect("write");
    fs::write(root.join("gone.md"), "the run's too\n").expect("write");
    record(&root, None, RUN, Side::After).await.expect("after");
    fs::write(root.join("keep.md"), "a person's, since\n").expect("write");

    let done = restore(&root, None, RUN).await.expect("restore");

    assert_eq!(done.kept, vec!["keep.md".to_owned()]);
    assert_eq!(done.restored, vec!["gone.md".to_owned()]);
    assert_eq!(
        fs::read_to_string(root.join("keep.md")).expect("read"),
        "a person's, since\n"
    );
}

/// A resumed run keeps the tree from before any of it; its `after` moves.
#[tokio::test]
async fn a_second_before_keeps_the_first() {
    let Some((_dir, root)) = repository() else {
        return;
    };
    let first = record(&root, None, RUN, Side::Before)
        .await
        .expect("before");
    fs::write(root.join("keep.md"), "halfway\n").expect("write");
    record(&root, None, RUN, Side::After).await.expect("after");
    let again = record(&root, None, RUN, Side::Before).await.expect("again");
    fs::write(root.join("keep.md"), "finished\n").expect("write");
    record(&root, None, RUN, Side::After).await.expect("after");

    assert_eq!(first, again);
    let checkpoint = read(&root, None, RUN).await.expect("read").expect("some");
    assert!(
        checkpoint.patch.contains("+finished"),
        "{}",
        checkpoint.patch
    );
    assert!(!checkpoint.patch.contains("halfway"));
}

/// A run still going has a `before` and nothing to restore yet.
#[tokio::test]
async fn a_run_without_an_after_cannot_be_restored() {
    let Some((_dir, root)) = repository() else {
        return;
    };
    record(&root, None, RUN, Side::Before)
        .await
        .expect("before");

    let checkpoint = read(&root, None, RUN).await.expect("read").expect("some");
    assert!(checkpoint.after_at.is_empty());
    assert!(checkpoint.changes.is_empty());
    assert!(restore(&root, None, RUN).await.is_err());
}

/// A fresh `git init` has no `HEAD` and no index; a checkpoint still works,
/// and still commits nothing on a branch.
#[tokio::test]
async fn a_repository_with_no_commit_is_checkpointed_too() {
    let (_dir, root) = folder();
    if super::super::program(&root).is_err() {
        return;
    }
    git(&root, &["init", "-q"]);

    record(&root, None, RUN, Side::Before)
        .await
        .expect("before");
    fs::write(root.join("first.md"), "hello\n").expect("write");
    record(&root, None, RUN, Side::After).await.expect("after");

    let checkpoint = read(&root, None, RUN).await.expect("read").expect("some");
    assert_eq!(checkpoint.changes.len(), 1);
    assert_eq!(git(&root, &["branch", "--list"]), "", "no branch appeared");
    restore(&root, None, RUN).await.expect("restore");
    assert!(!root.join("first.md").exists());
}

/// Ignored files are not captured, and so never restored or removed.
#[tokio::test]
async fn ignored_files_are_not_part_of_a_checkpoint() {
    let Some((_dir, root)) = repository() else {
        return;
    };
    fs::write(root.join(".gitignore"), "*.log\n").expect("write");
    git(&root, &["add", ".gitignore"]);
    git(&root, &["commit", "-q", "-m", "ignore logs"]);

    record(&root, None, RUN, Side::Before)
        .await
        .expect("before");
    fs::write(root.join("run.log"), "noise\n").expect("write");
    record(&root, None, RUN, Side::After).await.expect("after");

    let checkpoint = read(&root, None, RUN).await.expect("read").expect("some");
    assert!(checkpoint.changes.is_empty());
    restore(&root, None, RUN).await.expect("restore");
    assert!(root.join("run.log").exists());
}

#[tokio::test]
async fn an_unversioned_folder_has_no_checkpoint() {
    let (_dir, root) = folder();
    mark(&root, None, RUN, Side::Before).await;
    assert_eq!(read(&root, None, RUN).await, Ok(None));
    assert!(restore(&root, None, RUN).await.is_err());
}

#[test]
fn a_run_id_is_one_ref_component() {
    assert!(ref_name(RUN, Side::Before).is_ok());
    for bad in ["", "../heads/main", "a/b", "a b", "x.lock", "a:b"] {
        assert!(ref_name(bad, Side::After).is_err(), "{bad}");
    }
}

/// Retention: beyond the newest runs, or older than the window, a run's refs
/// both go.
#[tokio::test]
async fn pruning_keeps_the_newest_runs_inside_the_window() {
    let Some((_dir, root)) = repository() else {
        return;
    };
    for run in ["run-a", "run-b", "run-c"] {
        record(&root, None, run, Side::Before)
            .await
            .expect("before");
        record(&root, None, run, Side::After).await.expect("after");
    }
    let git_ = Git::new(&root, None);
    let now = chrono::Utc::now().timestamp();

    assert_eq!(git_.prune(now, 10, KEEP_DAYS).await, Ok(0));
    assert_eq!(git_.prune(now, 2, KEEP_DAYS).await, Ok(1));
    let left = git(&root, &["for-each-ref", "--format=%(refname)", REF_ROOT]);
    assert_eq!(left.lines().count(), 4, "{left}");

    let later = now + (KEEP_DAYS + 1) * 24 * 60 * 60;
    assert_eq!(git_.prune(later, 10, KEEP_DAYS).await, Ok(2));
    assert_eq!(
        git(&root, &["for-each-ref", "--format=%(refname)", REF_ROOT]),
        ""
    );
}

#[test]
fn only_an_identity_that_can_write_is_checkpointed() {
    assert!(may_write(&["fs_read".to_owned(), "fs_write".to_owned()]));
    assert!(may_write(&["shell_exec".to_owned()]));
    assert!(!may_write(&[
        "fs_read".to_owned(),
        "skill_return".to_owned()
    ]));
}
