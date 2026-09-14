//! Containment, against a real filesystem.
//!
//! PLAN 5.1 names path containment as the likeliest place for a bug in the
//! whole project, so these tests are written as the attacks they are guarding
//! against rather than as a walk through the implementation: climb out with
//! `..`, climb out through a link, pretend to be inside, spell the same
//! directory a different way.
//!
//! Everything here runs against real directories in a temporary tree, because
//! the interesting half of the behaviour only exists once symlinks do.

use std::fs;
use std::path::{Path, PathBuf};

use aegis_lib::policy::path::{is_contained, resolve, PathError};
use tempfile::TempDir;

/// A canonical temporary directory.
///
/// Canonical matters: on macOS `/var` is a link to `/private/var`, and a
/// workspace root that has not been resolved would make every path inside it
/// look like an escape.
fn temp() -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("temp dir");
    let path = dunce::canonicalize(dir.path()).expect("canonical temp dir");
    (dir, path)
}

/// Creates a directory link, or reports that this machine will not make one.
///
/// Windows needs Developer Mode or an elevated process to create a symlink, so
/// these tests announce a skip instead of failing on a machine that has
/// neither. On CI with either enabled, they run.
fn link_dir(target: &Path, link: &Path) -> bool {
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_dir(target, link);
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(target, link);

    if made.is_err() {
        eprintln!("skipping: this machine does not allow creating symlinks");
        return false;
    }
    true
}

/// Creates a file link, or reports that this machine will not make one.
fn link_file(target: &Path, link: &Path) -> bool {
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_file(target, link);
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(target, link);

    if made.is_err() {
        eprintln!("skipping: this machine does not allow creating symlinks");
        return false;
    }
    true
}

#[test]
fn the_workspace_root_is_inside_itself() {
    let (_guard, ws) = temp();

    let resolved = resolve(&ws, ".").expect("the root resolves");

    assert!(resolved.inside);
    assert_eq!(resolved.path, ws);
}

#[test]
fn a_relative_argument_is_taken_against_the_workspace() {
    let (_guard, ws) = temp();
    fs::create_dir(ws.join("src")).expect("mkdir");
    fs::write(ws.join("src/main.rs"), "fn main() {}").expect("write");

    let resolved = resolve(&ws, "src/main.rs").expect("resolves");

    assert!(resolved.inside);
    assert_eq!(resolved.path, ws.join("src").join("main.rs"));
}

#[test]
fn dot_dot_that_stays_inside_is_still_inside() {
    let (_guard, ws) = temp();
    fs::create_dir(ws.join("a")).expect("mkdir");
    fs::create_dir(ws.join("b")).expect("mkdir");

    let resolved = resolve(&ws, "a/../b").expect("resolves");

    assert!(resolved.inside);
    assert!(resolved.looked_inside);
    assert_eq!(resolved.path, ws.join("b"));
}

#[test]
fn dot_dot_that_climbs_out_is_outside_and_does_not_pretend_otherwise() {
    let (_guard, ws) = temp();

    let resolved = resolve(&ws, "../../etc/passwd").expect("resolves");

    assert!(!resolved.inside);
    assert!(
        !resolved.looked_inside,
        "a plain `..` climb is visibly outside, so it is an ask and not an escape"
    );
    assert!(!resolved.escaped());
}

#[test]
fn an_absolute_path_elsewhere_is_outside() {
    let (_guard, ws) = temp();
    let (_other_guard, other) = temp();

    let resolved = resolve(&ws, &other.to_string_lossy()).expect("resolves");

    assert!(!resolved.inside);
    assert!(!resolved.escaped());
}

#[test]
fn a_path_that_does_not_exist_yet_still_resolves() {
    let (_guard, ws) = temp();

    let resolved = resolve(&ws, "generated/report.md").expect("resolves");

    assert!(
        resolved.inside,
        "fs_write has to be judgeable before the file exists"
    );
    assert_eq!(resolved.path, ws.join("generated").join("report.md"));
}

#[test]
fn a_missing_path_that_climbs_out_is_outside() {
    let (_guard, ws) = temp();

    let resolved = resolve(&ws, "nope/../../elsewhere/report.md").expect("resolves");

    assert!(!resolved.inside);
}

#[test]
fn a_link_out_of_the_workspace_is_an_escape() {
    let (_guard, ws) = temp();
    let (_other_guard, other) = temp();
    fs::write(other.join("secret.txt"), "s3cret").expect("write");

    if !link_dir(&other, &ws.join("escape")) {
        return;
    }

    let resolved = resolve(&ws, "escape/secret.txt").expect("resolves");

    assert!(!resolved.inside, "the link leads out of the workspace");
    assert!(
        resolved.looked_inside,
        "read as text, the argument is a path inside the workspace"
    );
    assert!(
        resolved.escaped(),
        "that gap is exactly what makes this a hard denial"
    );
    assert_eq!(resolved.path, other.join("secret.txt"));
}

#[test]
fn dot_dot_after_a_link_follows_the_real_tree() {
    let (_guard, ws) = temp();
    let (_other_guard, other) = temp();
    let nested = other.join("nested");
    fs::create_dir(&nested).expect("mkdir");
    fs::write(other.join("secret.txt"), "s3cret").expect("write");

    if !link_dir(&nested, &ws.join("hop")) {
        return;
    }

    // Lexically this reads as `<ws>/secret.txt`. Popping a *resolved* path
    // instead lands where the operating system would: beside the link target.
    let resolved = resolve(&ws, "hop/../secret.txt").expect("resolves");

    assert_eq!(resolved.path, other.join("secret.txt"));
    assert!(!resolved.inside);
    assert!(resolved.escaped());
}

#[test]
fn a_link_that_stays_inside_the_workspace_is_inside() {
    let (_guard, ws) = temp();
    let inner = ws.join("real");
    fs::create_dir(&inner).expect("mkdir");
    fs::write(inner.join("note.txt"), "hello").expect("write");

    if !link_dir(&inner, &ws.join("alias")) {
        return;
    }

    let resolved = resolve(&ws, "alias/note.txt").expect("resolves");

    assert!(resolved.inside);
    assert!(!resolved.escaped());
    assert_eq!(resolved.path, inner.join("note.txt"));
}

#[test]
fn a_dangling_link_does_not_resolve() {
    let (_guard, ws) = temp();

    if !link_file(&ws.join("gone.txt"), &ws.join("broken.txt")) {
        return;
    }

    assert_eq!(resolve(&ws, "broken.txt"), Err(PathError::Unresolvable));
}

#[test]
fn an_empty_argument_is_refused() {
    let (_guard, ws) = temp();

    assert_eq!(resolve(&ws, ""), Err(PathError::Empty));
    assert_eq!(resolve(&ws, "   \t "), Err(PathError::Empty));
}

#[test]
fn containment_compares_components_not_text() {
    let (_guard, ws) = temp();
    let sibling = {
        let mut path = ws.clone().into_os_string();
        path.push("-next-door");
        PathBuf::from(path)
    };

    assert!(
        !is_contained(&ws, &sibling),
        "`{}` must not contain `{}`",
        ws.display(),
        sibling.display()
    );
}

#[cfg(windows)]
mod windows {
    use super::*;

    #[test]
    fn a_verbatim_prefix_names_the_same_directory() {
        assert!(is_contained(
            Path::new(r"C:\ws"),
            Path::new(r"\\?\C:\ws\src\main.rs")
        ));
        assert!(is_contained(
            Path::new(r"\\?\C:\ws"),
            Path::new(r"C:\ws\src\main.rs")
        ));
    }

    #[test]
    fn comparison_folds_case_because_the_filesystem_does() {
        assert!(is_contained(Path::new(r"C:\ws"), Path::new(r"C:\WS\Src")));
        assert!(is_contained(Path::new(r"c:\ws"), Path::new(r"C:\ws\src")));
    }

    #[test]
    fn a_unc_share_is_an_ordinary_root() {
        let root = Path::new(r"\\server\share\ws");

        assert!(is_contained(root, Path::new(r"\\server\share\ws\src")));
        assert!(is_contained(
            root,
            Path::new(r"\\?\UNC\server\share\ws\src")
        ));
        assert!(
            !is_contained(root, Path::new(r"\\server\other\ws\src")),
            "a different share is a different root"
        );
        assert!(
            !is_contained(root, Path::new(r"C:\ws\src")),
            "a local drive is not the share"
        );
    }

    #[test]
    fn a_relative_path_may_not_re_root_the_join() {
        let (_guard, ws) = temp();

        // `C:\ws` joined with `\windows` is `C:\windows`, which reads as if it
        // were relative and is not.
        assert_eq!(
            resolve(&ws, r"\windows\system32"),
            Err(PathError::RootedRelative)
        );
        assert_eq!(resolve(&ws, "C:tmp"), Err(PathError::RootedRelative));
    }

    #[test]
    fn a_segment_windows_would_respell_is_refused() {
        let (_guard, ws) = temp();

        for raw in [
            r".git.\hooks",
            r"world \ESSENCE.md",
            "notes.md:stream",
            r"a\b.",
            "credentials. ",
        ] {
            assert_eq!(resolve(&ws, raw), Err(PathError::WindowsName), "{raw}");
        }
    }

    #[test]
    fn an_existing_folder_is_spelled_the_way_the_disk_spells_it() {
        let (_guard, ws) = temp();
        fs::create_dir_all(ws.join(".git").join("hooks")).expect("mkdir");
        fs::create_dir(ws.join("Src")).expect("mkdir");

        let cased = resolve(&ws, r"SRC\new.rs").expect("resolves");
        assert_eq!(cased.path, ws.join("Src").join("new.rs"));

        if ws.join("GIT~1").exists() {
            let short = resolve(&ws, r"GIT~1\hooks\pre-commit").expect("resolves");
            assert_eq!(short.path, ws.join(".git").join("hooks").join("pre-commit"));
        } else {
            eprintln!("skipping the short-name half: this volume does not generate 8.3 names");
        }
    }
}

#[cfg(not(windows))]
mod unix {
    use super::*;

    #[test]
    fn comparison_keeps_case_because_the_filesystem_may() {
        assert!(!is_contained(Path::new("/ws"), Path::new("/WS/src")));
        assert!(is_contained(Path::new("/ws"), Path::new("/ws/src")));
    }
}
