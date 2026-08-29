//! Path resolution and workspace containment.
//!
//! This is the single place that answers "where does this argument actually
//! point, and is that inside the workspace?". Nothing else in the runtime is
//! allowed to reason about a path the model supplied — the tools take
//! already-resolved [`PathBuf`]s from a [`Decision`](crate::policy::Decision),
//! so a containment check cannot be forgotten in one tool and remembered in
//! another. PLAN 5.1 calls this the likeliest place for a containment bug in
//! the whole project, which is why it lands a phase ahead of the tools.
//!
//! Three properties are what the rest of the policy layer relies on:
//!
//! * **Symlinks are resolved before containment is judged.** A path is walked
//!   component by component, and every component that turns out to be a link
//!   is canonicalized on the spot. `..` then pops a *resolved* path, which is
//!   what the operating system would do — popping a lexical path instead is
//!   the classic way to walk out of a jail through a link.
//! * **A path that does not exist yet still resolves.** `fs_write` creates
//!   files, so refusing to reason about a missing target would make writes
//!   unjudgeable. Components that exist are resolved; the remaining tail is
//!   kept literally, and containment is decided on the whole.
//! * **The lexical answer is kept alongside the real one.** When an argument
//!   *looks* contained but resolves outside, that is a symlink escape, and
//!   PLAN 3.2 makes it a hard denial rather than a prompt: there is no honest
//!   way to describe it to a user in an approval dialog.
//!
//! Windows carries most of the sharp edges (PLAN 5.1). Verbatim `\\?\` paths
//! are simplified before anything compares them, UNC roots (`\\server\share`)
//! are ordinary prefixes here, and comparison is case-insensitive because the
//! filesystem is.

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
    /// A relative argument carried a root or a drive prefix (`\tmp`, `C:tmp`).
    ///
    /// Joining one of these onto the workspace does not do what it reads like:
    /// on Windows `C:\ws` joined with `\tmp` is `C:\tmp`, silently outside.
    /// Refusing is the only safe reading.
    RootedRelative,
    /// A link on the way could not be followed — dangling, looping, or
    /// unreadable.
    Unresolvable,
}

impl PathError {
    /// The reason, in words that belong in an error a user reads.
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Empty => "no path was given",
            Self::RootedRelative => "a relative path may not start at a drive or a filesystem root",
            Self::Unresolvable => "the path could not be resolved",
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
    /// Whether the argument was contained when read *lexically*, before any
    /// link was followed.
    ///
    /// `looked_inside && !inside` is a symlink escape. It is deliberately not
    /// folded into `inside`: the two differ exactly where the interesting
    /// attack is, and the caller treats that difference as a hard denial.
    pub looked_inside: bool,
}

impl Resolved {
    /// Whether the argument pretended to be contained and was not.
    pub const fn escaped(&self) -> bool {
        self.looked_inside && !self.inside
    }

    /// The path relative to `workspace`, when it is contained.
    ///
    /// Used by the parts of policy that must not read the user's own choice of
    /// workspace location — the sensitive-name predicate, and `.git/`
    /// detection.
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

/// Resolves `raw` against `workspace`.
///
/// `workspace` must already be canonical and absolute — it comes from
/// `store::canonical_workspace`, which is the only way a workspace enters the
/// runtime. A relative `raw` is taken as relative to the workspace root, which
/// is the convention the model is told about in the system prompt.
pub fn resolve(workspace: &Path, raw: &str) -> Result<Resolved, PathError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(PathError::Empty);
    }

    let given = dunce::simplified(Path::new(trimmed));
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
    let path = walk(&joined)?;
    let inside = is_contained(workspace, &path);

    Ok(Resolved {
        path,
        inside,
        looked_inside,
    })
}

/// Whether `candidate` is `root` itself or below it.
///
/// Compared component by component rather than as strings, so `C:\ws2` is not
/// "inside" `C:\ws` — a prefix test on the text would say it is. Both sides
/// are simplified first, which is what makes a verbatim `\\?\C:\ws\a` and a
/// plain `C:\ws` agree.
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

/// Resolves a path against the filesystem, one component at a time.
///
/// The invariant is that `out` is always fully resolved: every component
/// pushed so far either exists and is not a link, or was replaced by the
/// canonical path of the link's target. That is what makes the `ParentDir`
/// arm correct — popping a resolved path follows the real tree, while popping
/// a lexical one would undo a link traversal the OS would not have undone.
///
/// A component that does not exist is kept as written: nothing below a missing
/// directory can be a link either, so there is nothing left to resolve.
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

/// Compares two path components, with roots compared by what they mean.
///
/// `dunce::simplified` unwraps a verbatim *disk* path but leaves a verbatim
/// UNC one alone, because `\\?\UNC\server\share` cannot always be rewritten as
/// `\\server\share` safely. Comparing the two as text would then say a share
/// does not contain itself. Comparing the parsed prefix instead sidesteps the
/// question: `Disk` and `VerbatimDisk` are the same drive, `UNC` and
/// `VerbatimUNC` are the same share, and neither spelling has to win.
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

/// Case-insensitive on Windows, exact everywhere else.
///
/// Windows filesystems fold case, so `C:\WS` and `c:\ws` name one directory
/// and a case-sensitive comparison would report a contained path as an escape.
/// Folding is done on the whole string rather than ASCII-only, because
/// non-ASCII directory names are ordinary. A path that is not valid Unicode
/// falls back to an exact comparison — it cannot be folded, and both sides of
/// a real comparison come from the same filesystem anyway.
#[cfg(windows)]
fn os_eq(a: &OsStr, b: &OsStr) -> bool {
    match (a.to_str(), b.to_str()) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b) || a.to_lowercase() == b.to_lowercase(),
        _ => a == b,
    }
}

/// Exact comparison: these filesystems distinguish case.
///
/// macOS is the awkward middle — HFS+ and the default APFS volume fold case, a
/// case-sensitive APFS volume does not, and there is no way to know which one a
/// path is on without asking the volume. Comparing exactly is the conservative
/// direction: the failure mode is an extra approval prompt, never a
/// containment hole.
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
