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
