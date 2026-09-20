use super::*;

use tempfile::TempDir;

fn workspace() -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("temp dir");
    let root = dir.path().join("work");
    fs::create_dir_all(&root).expect("workspace dir");
    let root = dunce::canonicalize(&root).expect("canonical workspace");
    (dir, root)
}

fn names(listing: &TreeListing) -> Vec<&str> {
    listing
        .entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect()
}

fn write(root: &Path, rel: &str, bytes: &[u8]) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent dir");
    }
    fs::write(path, bytes).expect("write");
}

#[test]
fn the_cabinet_is_shown_and_the_repositorys_store_is_not() {
    let (_dir, root) = workspace();
    fs::create_dir_all(root.join(".git")).expect("a repository");
    fs::create_dir_all(root.join("node_modules/left-pad")).expect("dependencies");
    write(&root, ".aegis/briefs/README.md", b"# briefs");
    write(&root, "world/essence.md", b"# essence");
    write(&root, "data.csv", b"a,b\n");

    let listing = list(&root, None, false).expect("listed");

    assert_eq!(names(&listing), [".aegis", "world", "data.csv"]);
    assert_eq!(listing.hidden, 2, ".git and node_modules");
    assert_eq!(listing.entries[0].zone, Zone::Cabinet);
    assert_eq!(listing.entries[1].zone, Zone::World);
    assert_eq!(listing.entries[2].bytes, Some(4));
}

#[test]
fn gitignored_entries_are_hidden_by_default_and_marked_when_asked_for() {
    let (_dir, root) = workspace();
    fs::create_dir_all(root.join(".git")).expect("a repository");
    write(&root, ".gitignore", b"build/\n*.log\n");
    write(&root, "build/out.bin", b"x");
    write(&root, "debug.log", b"x");
    write(&root, "notes.md", b"x");

    let hiding = list(&root, None, false).expect("listed");
    assert_eq!(names(&hiding), [".gitignore", "notes.md"]);
    assert_eq!(hiding.hidden, 3);

    let showing = list(&root, None, true).expect("listed");
    assert_eq!(showing.hidden, 0);
    let ignored: Vec<&str> = showing
        .entries
        .iter()
        .filter(|entry| entry.ignored)
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(ignored, [".git", "build", "debug.log"]);
}

/// A rule written at the root still applies to a folder listed on its own.
#[test]
fn a_subfolder_is_listed_under_the_roots_ignore_rules() {
    let (_dir, root) = workspace();
    fs::create_dir_all(root.join(".git")).expect("a repository");
    write(&root, ".gitignore", b"*.tmp\n");
    write(&root, "src/keep.rs", b"x");
    write(&root, "src/scratch.tmp", b"x");

    let listing = list(&root, Some("src"), false).expect("listed");

    assert_eq!(names(&listing), ["keep.rs"]);
    assert_eq!(listing.dir, "src");
    assert_eq!(listing.entries[0].path, "src/keep.rs");
}

#[test]
fn everything_inside_an_ignored_folder_is_marked_ignored() {
    let (_dir, root) = workspace();
    fs::create_dir_all(root.join(".git")).expect("a repository");
    write(&root, ".gitignore", b"target/\n");
    write(&root, "target/debug/app", b"x");

    let listing = list(&root, Some("target"), true).expect("listed");

    assert_eq!(names(&listing), ["debug"]);
    assert!(listing.entries[0].ignored);
}

/// A `.gitignore` in a folder that is not a work tree names nothing, which
/// is what `git` itself would say about it.
#[test]
fn outside_a_work_tree_only_the_two_fixed_names_are_hidden() {
    let (_dir, root) = workspace();
    write(&root, ".gitignore", b"*.log\n");
    write(&root, "debug.log", b"x");
    fs::create_dir_all(root.join("node_modules")).expect("dependencies");

    let listing = list(&root, None, false).expect("listed");

    assert_eq!(names(&listing), [".gitignore", "debug.log"]);
    assert_eq!(listing.hidden, 1);
}

#[test]
fn folders_come_first_and_names_sort_without_case() {
    let (_dir, root) = workspace();
    write(&root, "b.txt", b"x");
    write(&root, "A.txt", b"x");
    write(&root, "zeta/x", b"x");

    assert_eq!(
        names(&list(&root, None, false).expect("listed")),
        ["zeta", "A.txt", "b.txt"]
    );
}

#[test]
fn a_huge_folder_is_capped_and_says_how_much_is_missing() {
    let (_dir, root) = workspace();
    for n in 0..LISTING_MAX_ENTRIES + 5 {
        write(&root, &format!("many/{n:05}.txt"), b"");
    }

    let listing = list(&root, Some("many"), false).expect("listed");

    assert_eq!(listing.entries.len(), LISTING_MAX_ENTRIES);
    assert_eq!(listing.more, 5);
}

#[test]
fn a_listing_cannot_be_aimed_outside_the_workspace() {
    let (dir, root) = workspace();
    let elsewhere = dunce::canonicalize(dir.path()).expect("canonical");

    assert!(matches!(
        list(&root, Some(".."), false),
        Err(AppError::RevealOutside { .. })
    ));
    assert!(matches!(
        list(&root, Some(&elsewhere.to_string_lossy()), false),
        Err(AppError::RevealOutside { .. })
    ));
    assert!(matches!(
        preview(&root, "../secret.txt"),
        Err(AppError::RevealOutside { .. })
    ));
}

#[cfg(unix)]
#[test]
fn a_link_that_climbs_out_is_listed_as_outside_and_never_opened() {
    let (dir, root) = workspace();
    fs::write(dir.path().join("secret.txt"), "not yours").expect("write");
    std::os::unix::fs::symlink(dir.path().join("secret.txt"), root.join("link.txt"))
        .expect("symlink");

    let listing = list(&root, None, false).expect("listed");
    assert!(listing.entries[0].outside, "{listing:?}");
    assert!(matches!(
        preview(&root, "link.txt"),
        Err(AppError::RevealOutside { .. })
    ));
}

#[test]
fn zones_follow_the_convention_and_only_the_first_segment_is_the_world() {
    assert_eq!(Zone::of(".aegis/briefs"), Zone::Briefs);
    assert_eq!(Zone::of(".aegis/briefs/intake.csv"), Zone::Briefs);
    assert_eq!(Zone::of(".aegis/artefacts/report.md"), Zone::Artefacts);
    assert_eq!(Zone::of(".aegis/status/STATUS.md"), Zone::Cabinet);
    assert_eq!(Zone::of(".aegis"), Zone::Cabinet);
    assert_eq!(Zone::of(".aegis-old/briefs"), Zone::Plain);
    assert_eq!(Zone::of(".aegis/briefsx"), Zone::Cabinet);
    assert_eq!(Zone::of("world/essence.md"), Zone::World);
    assert_eq!(Zone::of("src/world/map.rs"), Zone::Plain);
    assert_eq!(Zone::of(""), Zone::Plain);
}

#[test]
fn markdown_is_text_that_says_it_is_markdown() {
    let (_dir, root) = workspace();
    write(
        &root,
        ".aegis/briefs/intake.md",
        "# Intake\n\ninputs: `data.csv`\n".as_bytes(),
    );

    let shown = preview(&root, ".aegis/briefs/intake.md").expect("previewed");

    assert_eq!(shown.name, "intake.md");
    assert_eq!(shown.zone, Zone::Briefs);
    assert_eq!(
        shown.body,
        PreviewBody::Text {
            text: "# Intake\n\ninputs: `data.csv`\n".to_owned(),
            truncated: false,
            markdown: true,
        }
    );
}

#[test]
fn a_long_file_is_cut_and_says_so() {
    let (_dir, root) = workspace();
    write(&root, "long.txt", &vec![b'x'; TEXT_MAX_BYTES as usize + 10]);

    let shown = preview(&root, "long.txt").expect("previewed");

    match shown.body {
        PreviewBody::Text {
            text, truncated, ..
        } => {
            assert!(truncated);
            assert_eq!(text.len(), TEXT_MAX_BYTES as usize);
        }
        other => panic!("expected text, got {other:?}"),
    }
    assert_eq!(shown.bytes, TEXT_MAX_BYTES + 10);
}

#[test]
fn a_file_with_nul_bytes_is_binary_whatever_it_is_called() {
    let (_dir, root) = workspace();
    write(&root, "notes.md", b"looks like text\0but is not");

    assert_eq!(
        preview(&root, "notes.md").expect("previewed").body,
        PreviewBody::Binary { mime: None }
    );
}

#[test]
fn a_legacy_code_page_is_still_shown_as_text() {
    let (_dir, root) = workspace();
    // "café;prix" in Windows-1252.
    write(&root, "export.csv", b"caf\xe9;prix\n");

    match preview(&root, "export.csv").expect("previewed").body {
        PreviewBody::Text { text, .. } => assert!(text.starts_with("caf")),
        other => panic!("expected text, got {other:?}"),
    }
}

#[test]
fn an_image_is_known_by_its_bytes_not_its_name() {
    let (_dir, root) = workspace();
    let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
    write(&root, "shot.dat", png);
    write(&root, "fake.png", b"just words");

    assert_eq!(
        preview(&root, "shot.dat").expect("previewed").body,
        PreviewBody::Image {
            mime: "image/png".to_owned()
        }
    );
    let (bytes, mime) = image(&root, "shot.dat").expect("bytes");
    assert_eq!(bytes, png);
    assert_eq!(mime, "image/png");

    assert!(matches!(
        image(&root, "fake.png"),
        Err(AppError::RevealPath { .. })
    ));
}

#[test]
fn svg_is_markup_and_is_previewed_as_text() {
    let (_dir, root) = workspace();
    write(
        &root,
        "logo.svg",
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
    );

    assert!(matches!(
        preview(&root, "logo.svg").expect("previewed").body,
        PreviewBody::Text {
            markdown: false,
            ..
        }
    ));
    assert!(image(&root, "logo.svg").is_err());
}

#[test]
fn a_pdf_is_named_and_not_shown() {
    let (_dir, root) = workspace();
    write(&root, "contract.pdf", b"%PDF-1.7\n...");

    assert_eq!(
        preview(&root, "contract.pdf").expect("previewed").body,
        PreviewBody::Binary {
            mime: Some("application/pdf".to_owned())
        }
    );
}

#[test]
fn a_folder_or_a_missing_file_is_not_a_preview() {
    let (_dir, root) = workspace();
    fs::create_dir_all(root.join("src")).expect("dir");

    assert!(matches!(
        preview(&root, "src"),
        Err(AppError::RevealPath { .. })
    ));
    assert!(matches!(
        preview(&root, "gone.md"),
        Err(AppError::RevealPath { .. })
    ));
    assert!(matches!(
        preview(&root, ""),
        Err(AppError::RevealPath { .. })
    ));
}

#[test]
fn paths_from_the_window_are_normalized_before_they_are_keyed() {
    assert_eq!(normalize("./.aegis\\briefs/"), ".aegis/briefs");
    assert_eq!(normalize("."), "");
    assert_eq!(normalize("  "), "");
    assert_eq!(
        normalize("../x"),
        "../x",
        "containment is reveal's to refuse"
    );
}
