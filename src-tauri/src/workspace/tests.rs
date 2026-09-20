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
    for path in [BRIEFS_DIR, ARTEFACTS_DIR, STATUS_FILE, DECISIONS_FILE] {
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
    assert_eq!(ARTEFACTS_DIR, CONVENTION[2].rel_dir());
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
            ".aegis/evals/",
        ]
    );
    assert!(report.kept.is_empty());
    let evals = root.join(".aegis/evals");
    assert!(evals.is_dir());
    assert_eq!(fs::read_dir(&evals).expect("listed").count(), 0, "no seed");
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

/// PLAN 7.11: scaffolding reports what the folder already is, and leaves
/// the initialising to the command that knows whose `git` may run here.
#[test]
fn scaffolding_reports_the_versioning_it_found_and_changes_none_of_it() {
    let (_dir, root) = workspace();
    assert_eq!(
        layout(&root).versioning.tree,
        git::WorkTree::Unversioned,
        "a folder nobody asked about has no repository"
    );

    let report = scaffold(&root).expect("scaffolded");

    assert_eq!(report.versioning.tree, git::WorkTree::Unversioned);
    assert!(!report.initialized);
    assert_eq!(report.problem, None);
    assert!(
        !root.join(".git").exists(),
        "and the files half spawns nothing"
    );
    assert!(layout(&root).complete, "the directories are there");
}

/// The other half, folded in the way the command folds it.
#[tokio::test]
async fn a_scaffolded_folder_is_versioned_by_the_press_that_made_it() {
    let (_dir, root) = workspace();

    let report = scaffold(&root)
        .expect("scaffolded")
        .versioned(git::ensure(&root, None).await);

    // A machine without `git` takes the other row of PLAN 7.11's table,
    // and the claim there is the same one: the directories were the job.
    if let Some(problem) = &report.problem {
        assert!(!report.initialized, "{problem}");
        assert_eq!(report.versioning.tree, git::WorkTree::Unversioned);
    } else {
        assert!(report.initialized, "{report:?}");
        assert_eq!(report.versioning.tree, git::WorkTree::Here);
        assert_eq!(layout(&root).versioning.tree, git::WorkTree::Here);
    }

    assert!(
        layout(&root).complete,
        "and the directories are there either way"
    );
}

/// Versioning is the only thing that press does about git. Nothing writes
/// a `.gitignore`, a remote or a commit — approving a write is not
/// approving a commit (PLAN 7.11).
#[tokio::test]
async fn scaffolding_writes_nothing_of_gits_beyond_the_repository() {
    let (_dir, root) = workspace();
    let report = scaffold(&root)
        .expect("scaffolded")
        .versioned(git::ensure(&root, None).await);

    assert!(report.problem.is_some() || report.initialized);
    assert!(!root.join(".gitignore").exists());
    assert!(!root.join(".gitattributes").exists());
    assert!(!root.join(".gitmodules").exists());
}

/// A workspace inside somebody's monorepo is already versioned by it, and
/// an inner `.git` would split that history.
#[tokio::test]
async fn scaffolding_inside_a_repository_reports_the_ancestor_and_nests_nothing() {
    let (dir, root) = workspace();
    fs::create_dir(dir.path().join(".git")).expect("a repository above");

    let report = scaffold(&root)
        .expect("scaffolded")
        .versioned(git::ensure(&root, None).await);

    assert!(!report.initialized);
    assert_eq!(report.problem, None);
    assert_eq!(report.versioning.tree, git::WorkTree::Ancestor);
    assert!(!root.join(".git").exists(), "no nested repository");
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
