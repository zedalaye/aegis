//! Path resolution and workspace containment (PLAN 3).
//!
//! The only place a model-supplied path is interpreted; tools receive the
//! resolved [`PathBuf`] from a [`Decision`](crate::policy::Decision).
//!
//! * **Links are resolved before containment is judged**, component by
//!   component, so `..` pops the resolved path the way the OS does.
//! * **A missing tail still resolves**, kept as written, so `fs_write` can be
//!   judged before its file exists.
//! * **The lexical answer is kept beside the real one**: looking contained and
//!   resolving outside is a link escape, a hard denial (PLAN 3.2).
//!
//! On Windows (PLAN 5.1) verbatim and UNC prefixes compare by meaning, case
//! folds, existing folders are respelled as the disk spells them, and segments
//! Win32 would silently rewrite are refused.

use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// Why an argument could not be turned into a path policy can judge.
///
/// Every variant is a hard denial upstream (`E_PATH_INVALID`): none of them
/// describes a path a user could meaningfully approve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathError {
    /// The argument was empty, or only whitespace.
    Empty,
    /// A relative argument carried a root or a drive prefix (`\tmp`, `C:tmp`),
    /// which joined onto the workspace would leave it.
    RootedRelative,
    /// A link on the way could not be followed — dangling, looping, or
    /// unreadable.
    Unresolvable,
    /// A segment Win32 would respell: a trailing dot or space, stripped before
    /// opening (`.git.\hooks` opens `.git\hooks`), or a `:`, which names a data
    /// stream.
    WindowsName,
}

impl PathError {
    /// The reason, in words that belong in an error a user reads.
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Empty => "no path was given",
            Self::RootedRelative => "a relative path may not start at a drive or a filesystem root",
            Self::Unresolvable => "the path could not be resolved",
            Self::WindowsName => {
                "on Windows a path segment may not end with a dot or a space, or contain `:`"
            }
        }
    }
}

/// Where an argument points, and how that relates to the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The path to actually operate on: absolute, `..`-free, with every
    /// existing component's links followed.
    pub path: PathBuf,
    /// Whether [`Resolved::path`] is the workspace root or below it.
    pub inside: bool,
    /// Whether the argument was contained read lexically, before links were
    /// followed. `looked_inside && !inside` is a link escape.
    pub looked_inside: bool,
}

impl Resolved {
    /// Whether the argument pretended to be contained and was not.
    pub const fn escaped(&self) -> bool {
        self.looked_inside && !self.inside
    }

    /// The path relative to `workspace`, when contained. The name predicates
    /// read this, so the workspace's own location never matches them.
    pub fn relative_to(&self, workspace: &Path) -> Option<PathBuf> {
        if !self.inside {
            return None;
        }
        // `strip_prefix` compares components exactly, so it disagrees with the
        // case-insensitive containment used on Windows. Skipping the root's
        // component count is equivalent and does not depend on casing.
        let depth = workspace.components().count();
        Some(self.path.components().skip(depth).collect())
    }
}

/// Resolves `raw` against `workspace`, which must be canonical
/// (`store::canonical_workspace`). A relative `raw` is relative to the root.
pub fn resolve(workspace: &Path, raw: &str) -> Result<Resolved, PathError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(PathError::Empty);
    }

    let given = dunce::simplified(Path::new(trimmed));
    if !spelled_literally(given) {
        return Err(PathError::WindowsName);
    }
    let joined = if given.is_absolute() {
        given.to_path_buf()
    } else {
        // A component that anchors the path without making it absolute is the
        // `\tmp` / `C:tmp` case: it would re-root the join.
        if matches!(
            given.components().next(),
            Some(Component::Prefix(_) | Component::RootDir)
        ) {
            return Err(PathError::RootedRelative);
        }
        workspace.join(given)
    };

    let looked_inside = is_contained(workspace, &lexical(&joined));
    let path = on_disk(walk(&joined)?)?;
    let inside = is_contained(workspace, &path);

    Ok(Resolved {
        path,
        inside,
        looked_inside,
    })
}

/// Whether an already-resolved path still resolves to itself.
///
/// Checked right before a tool acts (PLAN 3.2): a folder turned into a link
/// while the dialog was open would redirect the call. This narrows the race to
/// two system calls; it is not handle-based.
pub fn unchanged(resolved: &Path) -> bool {
    walk(resolved)
        .and_then(on_disk)
        .is_ok_and(|again| same_path(&again, resolved))
}

/// Component-wise equality, with the same folding [`is_contained`] uses.
fn same_path(a: &Path, b: &Path) -> bool {
    let a = dunce::simplified(a);
    let b = dunce::simplified(b);
    a.components().count() == b.components().count()
        && a.components()
            .zip(b.components())
            .all(|(one, two)| segments_eq(one, two))
}

/// Whether every segment means what it says to Win32
/// ([`PathError::WindowsName`]). Checked on the argument: resolution would
/// hide the respelling.
#[cfg(windows)]
fn spelled_literally(path: &Path) -> bool {
    path.components().all(|component| match component {
        Component::Normal(name) => name
            .to_str()
            .is_none_or(|name| !(name.ends_with('.') || name.ends_with(' ') || name.contains(':'))),
        _ => true,
    })
}

/// Every segment means what it says: these filesystems respell nothing.
#[cfg(not(windows))]
const fn spelled_literally(_path: &Path) -> bool {
    true
}

/// Respells the existing part of a path as the disk does (`GIT~1` → `.git`,
/// `SRC` → `src`), so the name predicates read real names. The missing tail is
/// kept as written.
#[cfg(windows)]
fn on_disk(path: PathBuf) -> Result<PathBuf, PathError> {
    let mut missing = Vec::new();
    let mut existing = path.as_path();
    while fs::symlink_metadata(existing).is_err() {
        let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else {
            return Ok(path.clone());
        };
        missing.push(name);
        existing = parent;
    }

    let mut spelled = dunce::canonicalize(existing).map_err(|err| {
        tracing::debug!(%err, "a folder on the path could not be canonicalized");
        PathError::Unresolvable
    })?;
    spelled.extend(missing.into_iter().rev());
    Ok(spelled)
}

/// The path as walked: off Windows, a name is the only name.
#[cfg(not(windows))]
#[allow(clippy::unnecessary_wraps)]
fn on_disk(path: PathBuf) -> Result<PathBuf, PathError> {
    Ok(path)
}

/// Whether `candidate` is `root` or below it, compared component by component
/// (`C:\ws2` is not inside `C:\ws`) after simplifying verbatim prefixes.
pub fn is_contained(root: &Path, candidate: &Path) -> bool {
    let root = dunce::simplified(root);
    let candidate = dunce::simplified(candidate);

    // An empty root would contain everything, which is never what a caller
    // means; it can only come from a bug upstream.
    if root.as_os_str().is_empty() {
        return false;
    }

    let mut actual = candidate.components();
    root.components()
        .all(|expected| actual.next().is_some_and(|got| segments_eq(expected, got)))
}

/// Resolves a path one component at a time. `out` stays fully resolved — each
/// component exists and is not a link, or was replaced by its link's canonical
/// target — so `..` pops the real tree. Missing components are kept as written.
fn walk(path: &Path) -> Result<PathBuf, PathError> {
    let mut out = PathBuf::new();
    let mut depth = 0usize;

    for component in path.components() {
        match component {
            // `..` at the root stays at the root, exactly as the OS does.
            Component::ParentDir => {
                if depth > 0 && out.pop() {
                    depth -= 1;
                }
            }
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir => out.push(component.as_os_str()),
            Component::Normal(name) => {
                out.push(name);
                depth += 1;

                match fs::symlink_metadata(&out) {
                    Ok(meta) if meta.file_type().is_symlink() => {
                        out = dunce::canonicalize(&out).map_err(|err| {
                            tracing::debug!(%err, "a link on the path could not be followed");
                            PathError::Unresolvable
                        })?;
                        depth = out
                            .components()
                            .filter(|c| matches!(c, Component::Normal(_)))
                            .count();
                    }
                    // Exists and is not a link, or does not exist at all:
                    // either way the literal component is the resolved one.
                    _ => {}
                }
            }
        }
    }

    Ok(out)
}

/// Normalizes `.` and `..` textually, touching no filesystem.
///
/// This is what the argument *claims*, and the only use for it is spotting the
/// gap between the claim and the truth.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    let mut depth = 0usize;

    for component in path.components() {
        match component {
            Component::ParentDir => {
                if depth > 0 && out.pop() {
                    depth -= 1;
                }
            }
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir => out.push(component.as_os_str()),
            Component::Normal(name) => {
                out.push(name);
                depth += 1;
            }
        }
    }

    out
}

/// Compares two path components.
///
/// A `Normal` component can never spell a root or a prefix, so comparing the
/// raw text is enough to keep the kinds apart.
#[cfg(not(windows))]
fn segments_eq(a: Component<'_>, b: Component<'_>) -> bool {
    os_eq(a.as_os_str(), b.as_os_str())
}

/// Compares two path components, prefixes by meaning: `Disk` and
/// `VerbatimDisk` are one drive, `UNC` and `VerbatimUNC` one share (`dunce`
/// leaves verbatim UNC paths alone).
#[cfg(windows)]
fn segments_eq(a: Component<'_>, b: Component<'_>) -> bool {
    use std::path::Prefix::{Disk, VerbatimDisk, VerbatimUNC, UNC};

    let (Component::Prefix(a), Component::Prefix(b)) = (a, b) else {
        return os_eq(a.as_os_str(), b.as_os_str());
    };

    match (a.kind(), b.kind()) {
        (Disk(one) | VerbatimDisk(one), Disk(two) | VerbatimDisk(two)) => {
            one.eq_ignore_ascii_case(&two)
        }
        (
            UNC(server, share) | VerbatimUNC(server, share),
            UNC(other, other_share) | VerbatimUNC(other, other_share),
        ) => os_eq(server, other) && os_eq(share, other_share),
        _ => os_eq(a.as_os_str(), b.as_os_str()),
    }
}

/// Case-insensitive, beyond ASCII; exact for names that are not valid Unicode.
#[cfg(windows)]
fn os_eq(a: &OsStr, b: &OsStr) -> bool {
    match (a.to_str(), b.to_str()) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b) || a.to_lowercase() == b.to_lowercase(),
        _ => a == b,
    }
}

/// Exact. On a case-folding macOS volume that can only cost an extra prompt,
/// never a containment hole.
#[cfg(not(windows))]
fn os_eq(a: &OsStr, b: &OsStr) -> bool {
    a == b
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `C:\ws` on Windows, `/ws` elsewhere — a root that needs no filesystem.
    fn root() -> &'static Path {
        Path::new(if cfg!(windows) { r"C:\ws" } else { "/ws" })
    }

    #[test]
    fn a_directory_contains_itself() {
        assert!(is_contained(root(), root()));
    }

    #[test]
    fn a_sibling_sharing_a_prefix_is_not_contained() {
        let sibling = Path::new(if cfg!(windows) { r"C:\ws2\a" } else { "/ws2/a" });
        assert!(
            !is_contained(root(), sibling),
            "containment must compare components, not text"
        );
    }

    #[test]
    fn an_empty_root_contains_nothing() {
        assert!(!is_contained(Path::new(""), root()));
    }

    #[test]
    fn lexical_normalization_stops_at_the_root() {
        let climbed = Path::new(if cfg!(windows) {
            r"C:\ws\..\..\..\x"
        } else {
            "/ws/../../../x"
        });
        let expected = Path::new(if cfg!(windows) { r"C:\x" } else { "/x" });
        assert_eq!(lexical(climbed), expected);
    }

    #[test]
    fn empty_arguments_are_refused() {
        assert_eq!(resolve(root(), "   "), Err(PathError::Empty));
    }
}
