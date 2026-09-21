//! Narrow standing grants (PLAN 7.23): a write prefix and a command shape.
//!
//! Neither is a row of its own. A narrow grant only ever collapses an ask
//! whose row already offers the wide grant it narrows — [`Grant::FsWrite`] or
//! [`Grant::Shell`] — so every exclusion of those rows (outside the
//! workspace, `.git/`, `world/`, a proposal, a git line that is not read-only)
//! holds for it without being restated here.

use std::path::{Component, Path};

use super::grants::{names_a_path, program_name, shell_key};
use super::{path, Grant, ResolvedCall};

/// Any further arguments. Last only.
pub const TAIL: &str = "…";

/// One path inside the workspace.
pub const PATH: &str = "<path>";

/// Most literal words a proposed shape keeps before [`TAIL`]: the verb, and
/// what the verb acts on (`cargo test`, `pnpm run build`).
const PROPOSED_WORDS: usize = 2;

/// Programs whose ordinary job is to run code the workspace holds — build
/// scripts, package scripts, test files. A shape over one narrows what is
/// asked for, not what can happen (PLAN 7.23, *Stated plainly*).
const RUNS_WORKSPACE_CODE: &[&str] = &[
    "bash",
    "bun",
    "bundle",
    "cargo",
    "cmd",
    "composer",
    "deno",
    "dotnet",
    "go",
    "gradle",
    "gradlew",
    "just",
    "make",
    "mvn",
    "node",
    "npm",
    "npx",
    "php",
    "pip",
    "pnpm",
    "powershell",
    "pwsh",
    "py",
    "pytest",
    "python",
    "python3",
    "rake",
    "ruby",
    "sh",
    "tox",
    "uv",
    "yarn",
];

/// A write prefix, normalized: `/`-separated, no leading `./`, no trailing
/// `/`. Refuses what could never be a folder under the workspace, and
/// `.git/` and `world/`, which no write grant reaches.
///
/// A segment may end in one `*`, which matches any name starting with what
/// precedes it (`drafts-*`); `*` alone is any one folder.
pub fn prefix(raw: &str) -> Result<String, String> {
    let spelled = raw.trim().replace('\\', "/");
    let spelled = spelled.trim_end_matches('/');
    let spelled = spelled.strip_prefix("./").unwrap_or(spelled);

    if spelled.is_empty() || spelled == "." {
        return Err(
            "a write prefix names a folder inside the workspace, like `.aegis/artefacts`; the \
             whole workspace is `fs_write` itself"
                .to_owned(),
        );
    }
    if spelled.starts_with('/') || spelled.contains(':') {
        return Err(format!(
            "`{spelled}` is not relative to the workspace. A write prefix is a folder inside it, \
             like `.aegis/artefacts`"
        ));
    }

    let segments: Vec<&str> = spelled.split('/').collect();
    for (at, segment) in segments.iter().enumerate() {
        if segment.is_empty() || *segment == "." || *segment == ".." {
            return Err(format!(
                "`{spelled}` has an empty, `.` or `..` segment. Name the folder the way it is \
                 spelled from the workspace root"
            ));
        }
        if segment
            .find('*')
            .is_some_and(|star| star + 1 != segment.len())
        {
            return Err(format!(
                "`{segment}` in `{spelled}`: a segment may end in one `*`, and nowhere else"
            ));
        }
        if segment.eq_ignore_ascii_case(".git") {
            return Err(format!(
                "`{spelled}` is inside `.git/`, where the repository keeps its history. No write \
                 grant reaches it"
            ));
        }
        if at == 0 && segment.eq_ignore_ascii_case(crate::world::WORLD_DIR) {
            return Err(format!(
                "`{spelled}` is inside `world/`, the constitution. Amending it is a human \
                 decision, and no write grant reaches it"
            ));
        }
    }

    Ok(segments.join("/"))
}

/// A command shape's arguments, normalized: `...` is spelled [`TAIL`]. `None`
/// when the shape is every argument, which is [`Grant::Shell`] and says so.
pub fn shape(args: &[String]) -> Result<Option<Vec<String>>, String> {
    let tokens: Vec<String> = args
        .iter()
        .map(|token| match token.trim() {
            "..." => TAIL.to_owned(),
            _ => token.clone(),
        })
        .collect();

    if let Some(at) = tokens.iter().position(|token| token == TAIL) {
        if at + 1 != tokens.len() {
            return Err(format!(
                "`{TAIL}` stands for any further arguments, so it can only come last"
            ));
        }
    }
    if tokens.iter().any(|token| token.trim().is_empty()) {
        return Err("a shape's words are not empty".to_owned());
    }
    if tokens == [TAIL] {
        return Ok(None);
    }
    Ok(Some(tokens))
}

impl Grant {
    /// A write grant under one folder (PLAN 7.23). See [`prefix`].
    pub fn write_under(raw: &str) -> Result<Self, String> {
        Ok(Self::FsWriteUnder {
            prefix: prefix(raw)?,
        })
    }

    /// A command shape (PLAN 7.23): the program key of [`Grant::shell`] and a
    /// closed argument pattern. A pattern of [`TAIL`] alone is every
    /// argument, and comes back as [`Grant::Shell`].
    pub fn shape(program: &str, args: &[String]) -> Result<Self, String> {
        if program.trim().is_empty() {
            return Err("a shape names the program it runs".to_owned());
        }
        Ok(match shape(args)? {
            Some(args) => Self::ShellShape {
                program: shell_key(program),
                args,
            },
            None => Self::shell(program),
        })
    }

    /// Why this grant could not have been built by [`Grant::write_under`] or
    /// [`Grant::shape`], or `None`. A routine's grants are read back from a
    /// file somebody can edit.
    pub fn malformed(&self) -> Option<String> {
        match self {
            Self::FsWriteUnder { prefix: held } => match prefix(held) {
                Ok(normal) if normal == *held => None,
                Ok(normal) => Some(format!("write it as `{normal}`")),
                Err(reason) => Some(reason),
            },
            Self::ShellShape { program, args } => match Self::shape(program, args) {
                Ok(built) if built == *self => None,
                Ok(Self::Shell { .. }) => Some(format!(
                    "`{program} {TAIL}` is every argument of `{program}`: sign `{program}` itself \
                     if that is meant"
                )),
                Ok(_) => Some(format!("`{program}` is not spelled as its grant key")),
                Err(reason) => Some(reason),
            },
            _ => None,
        }
    }

    /// Whether this held grant collapses an ask whose row offered `offered`
    /// for `call`. Equal grants always do; a narrow one does when it narrows
    /// that very grant and the call fits inside it.
    pub fn covers(&self, offered: &Grant, call: &ResolvedCall, workspace: &Path) -> bool {
        if self == offered {
            return true;
        }
        match (self, offered, call) {
            (Self::FsWriteUnder { prefix }, Self::FsWrite, ResolvedCall::FsWrite { path, .. }) => {
                under(prefix, workspace, path)
            }
            (
                Self::ShellShape { program, args },
                Self::Shell { program: key },
                ResolvedCall::ShellExec {
                    args: given, cwd, ..
                },
            ) => program == key && fits(args, given, cwd, workspace),
            _ => false,
        }
    }

    /// The narrowest grant that still covers `call`, for an *allow standing*
    /// answer to a parked ask (PLAN 7.23): the file's folder rather than the
    /// workspace, `cargo test …` rather than `cargo`. Anything else, or a
    /// call nothing narrower describes, comes back unchanged.
    pub fn narrowed(self, call: &ResolvedCall, workspace: &Path) -> Self {
        match (&self, call) {
            (Self::FsWrite, ResolvedCall::FsWrite { path, .. }) => {
                let folder: Vec<&str> = relative(workspace, path)
                    .map(|segments| {
                        let mut segments = segments;
                        segments.pop();
                        segments
                    })
                    .unwrap_or_default();
                if folder.is_empty() {
                    return self;
                }
                Self::write_under(&folder.join("/")).unwrap_or(self)
            }
            (Self::Shell { program }, ResolvedCall::ShellExec { args, .. }) => {
                let words: Vec<String> = args
                    .iter()
                    .take_while(|word| is_plain_word(word))
                    .take(PROPOSED_WORDS)
                    .cloned()
                    .collect();
                if words.is_empty() && !args.is_empty() {
                    return self;
                }
                let mut pattern = words;
                if pattern.len() < args.len() {
                    pattern.push(TAIL.to_owned());
                }
                Self::ShellShape {
                    program: program.clone(),
                    args: pattern,
                }
            }
            _ => self,
        }
    }
}

/// `program args…`, as a shape reads.
pub fn shape_line(program: &str, args: &[String]) -> String {
    std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

/// What a shape's tokens mean, for its label.
pub fn shape_meaning(args: &[String]) -> &'static str {
    match (
        args.iter().any(|token| token == PATH),
        args.last().is_some_and(|token| token == TAIL),
    ) {
        (false, false) => "exactly that line; any other arguments are still asked about",
        (false, true) => "`…` is any further arguments, and any other line is still asked about",
        (true, false) => {
            "`<path>` is one path inside this workspace, and any other line is still asked about"
        }
        (true, true) => {
            "`<path>` is one path inside this workspace and `…` any further arguments; any other \
             line is still asked about"
        }
    }
}

/// Whether `program` (a grant key) runs code the workspace holds, which a
/// shape over it cannot narrow.
pub fn runs_workspace_code(program: &str) -> bool {
    names_a_path(program) || RUNS_WORKSPACE_CODE.contains(&program_name(program).as_str())
}

/// A word a proposed shape keeps literally: not an option, not a path.
fn is_plain_word(word: &str) -> bool {
    !word.is_empty()
        && !word.starts_with('-')
        && !word.contains(['/', '\\', '.', ':', '='])
        && !word.chars().any(char::is_whitespace)
        && word != TAIL
        && word != PATH
}

/// The components of `target` below `workspace`, when it is contained.
fn relative<'a>(workspace: &Path, target: &'a Path) -> Option<Vec<&'a str>> {
    if !path::is_contained(workspace, target) {
        return None;
    }
    // Skipping the root's component count, as `Resolved::relative_to` does,
    // so the comparison does not depend on how the root is cased.
    let depth = dunce::simplified(workspace).components().count();
    dunce::simplified(target)
        .components()
        .skip(depth)
        .map(|component| match component {
            Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .collect()
}

/// Whether `target` is strictly below the folder `prefix` names.
fn under(prefix: &str, workspace: &Path, target: &Path) -> bool {
    let Some(segments) = relative(workspace, target) else {
        return false;
    };
    let patterns: Vec<&str> = prefix.split('/').collect();
    segments.len() > patterns.len()
        && patterns
            .iter()
            .zip(&segments)
            .all(|(pattern, name)| segment_matches(pattern, name))
}

/// One prefix segment against one name. Case folds on Windows, as the
/// filesystem does.
fn segment_matches(pattern: &str, name: &str) -> bool {
    let (pattern, name) = if cfg!(windows) {
        (pattern.to_lowercase(), name.to_lowercase())
    } else {
        (pattern.to_owned(), name.to_owned())
    };
    match pattern.strip_suffix('*') {
        Some(stem) => name.starts_with(stem),
        None => name == pattern,
    }
}

/// Whether `given` fits the pattern `args`. A [`PATH`] is one argument that
/// is not an option and resolves inside the workspace, from `cwd`.
fn fits(pattern: &[String], given: &[String], cwd: &Path, workspace: &Path) -> bool {
    for (at, token) in pattern.iter().enumerate() {
        if token == TAIL {
            return true;
        }
        let Some(word) = given.get(at) else {
            return false;
        };
        let matched = if token == PATH {
            contained_path(word, cwd, workspace)
        } else {
            word == token
        };
        if !matched {
            return false;
        }
    }
    given.len() == pattern.len()
}

/// Whether one argument is a path inside the workspace.
fn contained_path(word: &str, cwd: &Path, workspace: &Path) -> bool {
    if word.trim().is_empty() || word.starts_with('-') {
        return false;
    }
    let joined = cwd.join(word);
    let Some(joined) = joined.to_str() else {
        return false;
    };
    path::resolve(workspace, joined).is_ok_and(|resolved| resolved.inside && !resolved.escaped())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &[&str]) -> Vec<String> {
        line.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn a_prefix_is_normalized_and_refuses_what_no_write_grant_reaches() {
        assert_eq!(
            prefix("./.aegis/artefacts/").as_deref(),
            Ok(".aegis/artefacts")
        );
        assert_eq!(
            prefix(r".aegis\artefacts").as_deref(),
            Ok(".aegis/artefacts")
        );
        assert_eq!(prefix("notes/drafts-*").as_deref(), Ok("notes/drafts-*"));
        assert_eq!(prefix("src/*/generated").as_deref(), Ok("src/*/generated"));

        for refused in [
            "",
            ".",
            "/etc",
            "C:/x",
            "../x",
            "a/../b",
            "a//b",
            "world",
            "World/x",
            ".git",
            "a/.git/hooks",
            "a*b",
            "**",
        ] {
            assert!(prefix(refused).is_err(), "{refused:?}");
        }
        assert!(
            prefix("src/world").is_ok(),
            "only the first segment is the world"
        );
    }

    #[test]
    fn a_shape_of_every_argument_is_the_program_grant() {
        assert_eq!(
            Grant::shape("cargo", &words(&["..."])),
            Ok(Grant::shell("cargo"))
        );
        assert_eq!(
            Grant::shape("cargo", &words(&["test", "..."])),
            Ok(Grant::ShellShape {
                program: "cargo".to_owned(),
                args: words(&["test", TAIL]),
            })
        );
        assert!(Grant::shape("cargo", &words(&[TAIL, "test"])).is_err());
        assert!(Grant::shape(" ", &words(&["test"])).is_err());
    }

    #[test]
    fn a_hand_edited_grant_is_malformed_unless_it_is_spelled_as_built() {
        let good = Grant::write_under(".aegis/artefacts").expect("a prefix");
        assert_eq!(good.malformed(), None);
        let spelled = Grant::FsWriteUnder {
            prefix: "./.aegis/artefacts/".to_owned(),
        };
        assert!(spelled.malformed().is_some());
        let world = Grant::FsWriteUnder {
            prefix: "world/x".to_owned(),
        };
        assert!(world.malformed().is_some());
        let every = Grant::ShellShape {
            program: "cargo".to_owned(),
            args: words(&[TAIL]),
        };
        assert!(every.malformed().is_some());
    }

    #[test]
    fn a_prefix_covers_writes_strictly_below_it() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let root = dunce::canonicalize(dir.path()).expect("canonical");
        let grant = Grant::write_under(".aegis/artefacts").expect("a prefix");
        let write = |relative: &str| ResolvedCall::FsWrite {
            path: root.join(relative),
            content: String::new(),
            create_dirs: true,
        };

        assert!(grant.covers(&Grant::FsWrite, &write(".aegis/artefacts/a.md"), &root));
        assert!(grant.covers(&Grant::FsWrite, &write(".aegis/artefacts/x/b.md"), &root));
        assert!(!grant.covers(&Grant::FsWrite, &write(".aegis/artefacts"), &root));
        assert!(!grant.covers(&Grant::FsWrite, &write(".aegis/artefacts-old/a.md"), &root));
        assert!(!grant.covers(&Grant::FsWrite, &write(".aegis/status/STATUS.md"), &root));
        assert!(
            !grant.covers(&Grant::WorldAmend, &write(".aegis/artefacts/a.md"), &root),
            "a prefix narrows `fs_write`'s row and no other"
        );

        let star = Grant::write_under("notes/drafts-*").expect("a prefix");
        assert!(star.covers(&Grant::FsWrite, &write("notes/drafts-1/a.md"), &root));
        assert!(!star.covers(&Grant::FsWrite, &write("notes/final/a.md"), &root));
    }

    #[test]
    fn a_shape_covers_its_lines_and_no_other() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let root = dunce::canonicalize(dir.path()).expect("canonical");
        std::fs::create_dir(root.join("src")).expect("mkdir");
        let offered = Grant::shell("cargo");
        let run = |line: &[&str]| ResolvedCall::ShellExec {
            program: "cargo".to_owned(),
            args: words(line),
            cwd: root.clone(),
            host: None,
            timeout_ms: None,
        };

        let test = Grant::shape("cargo", &words(&["test", TAIL])).expect("a shape");
        assert!(test.covers(&offered, &run(&["test"]), &root));
        assert!(test.covers(&offered, &run(&["test", "--lib", "policy"]), &root));
        assert!(!test.covers(&offered, &run(&["install", "ripgrep"]), &root));
        assert!(!test.covers(&offered, &run(&[]), &root));
        assert!(
            !test.covers(&Grant::shell("pnpm"), &run(&["test"]), &root),
            "a shape narrows its own program's row"
        );

        let exact = Grant::shape("cargo", &words(&["fmt"])).expect("a shape");
        assert!(exact.covers(&offered, &run(&["fmt"]), &root));
        assert!(!exact.covers(&offered, &run(&["fmt", "--all"]), &root));

        let path = Grant::shape("cargo", &words(&["check", PATH])).expect("a shape");
        assert!(path.covers(&offered, &run(&["check", "src"]), &root));
        assert!(!path.covers(&offered, &run(&["check", "--all"]), &root));
        assert!(!path.covers(&offered, &run(&["check", ".."]), &root));
        assert!(!path.covers(&offered, &run(&["check", "src", "x"]), &root));
    }

    #[test]
    fn a_parked_call_proposes_the_narrowest_grant_that_covers_it() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let root = dunce::canonicalize(dir.path()).expect("canonical");
        let write = |relative: &str| ResolvedCall::FsWrite {
            path: root.join(relative),
            content: String::new(),
            create_dirs: true,
        };
        assert_eq!(
            Grant::FsWrite.narrowed(&write(".aegis/artefacts/a.md"), &root),
            Grant::write_under(".aegis/artefacts").expect("a prefix")
        );
        assert_eq!(
            Grant::FsWrite.narrowed(&write("README.md"), &root),
            Grant::FsWrite,
            "a file at the root has no folder narrower than the workspace"
        );

        let run = |line: &[&str]| ResolvedCall::ShellExec {
            program: "cargo".to_owned(),
            args: words(line),
            cwd: root.clone(),
            host: None,
            timeout_ms: None,
        };
        let shape = |line: &[&str]| Grant::ShellShape {
            program: "cargo".to_owned(),
            args: words(line),
        };
        let cargo = Grant::shell("cargo");
        assert_eq!(
            cargo.clone().narrowed(&run(&["test", "--lib"]), &root),
            shape(&["test", TAIL])
        );
        assert_eq!(
            cargo.clone().narrowed(&run(&["run", "--bin", "x"]), &root),
            shape(&["run", TAIL])
        );
        assert_eq!(
            cargo.clone().narrowed(&run(&["fmt"]), &root),
            shape(&["fmt"])
        );
        assert_eq!(cargo.clone().narrowed(&run(&[]), &root), shape(&[]));
        assert_eq!(
            cargo.clone().narrowed(&run(&["--version"]), &root),
            cargo,
            "nothing narrower describes a line that starts with an option"
        );
    }

    #[test]
    fn a_program_that_runs_workspace_code_is_named_as_one() {
        assert!(runs_workspace_code("cargo"));
        assert!(runs_workspace_code("pnpm"));
        assert!(!runs_workspace_code("rg"));
        assert!(!runs_workspace_code("git"));
        assert!(runs_workspace_code("/ws/scripts/check"));
    }
}
