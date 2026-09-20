//! Finding the program: `PATH`, and `PATHEXT` on Windows (PLAN 5.1).

use super::*;

/// Turns the program a caller named into a path to spawn. Shared with
/// [`mcp::client`](crate::mcp::client), so `npx` resolves the same way in both.
/// Done here rather than by `Command`, which would resolve relative to this
/// process's directory instead of `cwd`.
pub(crate) fn resolve(program: &str, cwd: &Path) -> Result<PathBuf, String> {
    let program = program.trim();
    let named = Path::new(program);

    // A name with a separator in it is a path, not a PATH lookup — the same
    // rule every shell uses.
    if crate::policy::grants::names_a_path(program) {
        let candidate = if named.is_absolute() {
            named.to_path_buf()
        } else {
            cwd.join(named)
        };
        return executable(&candidate).ok_or_else(|| {
            format!(
                "`{program}` is not a program that can be run from {}",
                cwd.display()
            )
        });
    }

    for directory in search_path() {
        if directory.as_os_str().is_empty() {
            continue;
        }
        if let Some(found) = executable(&directory.join(named)) {
            return Ok(found);
        }
    }

    Err(not_found(program))
}

/// The directories PATH names, in order.
pub(super) fn search_path() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default()
}

/// Why a bare name found nothing.
pub(super) fn not_found(program: &str) -> String {
    #[cfg(windows)]
    if is_cmd_builtin(program) {
        return format!(
            "`{program}` is a cmd.exe builtin, not a program on PATH. Run `cmd` with args \
             [\"/c\", \"{program}\", …] if you meant the builtin — note that cmd will then parse \
             those arguments itself."
        );
    }

    format!("`{program}` is not a program on PATH")
}

/// Whether a candidate path names something this platform can execute.
#[cfg(unix)]
pub(super) fn executable(candidate: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;

    let meta = std::fs::metadata(candidate).ok()?;
    let runnable = meta.is_file() && meta.permissions().mode() & 0o111 != 0;
    runnable.then(|| candidate.to_path_buf())
}

/// Whether a candidate path names something this platform can execute: on
/// Windows by extension, trying `PATHEXT` in order, as a shell would
/// (PLAN 5.1).
#[cfg(windows)]
pub(super) fn executable(candidate: &Path) -> Option<PathBuf> {
    if candidate.extension().is_some() && candidate.is_file() {
        return Some(candidate.to_path_buf());
    }

    for extension in path_extensions() {
        let mut named = candidate.as_os_str().to_owned();
        named.push(&extension);
        let with_extension = PathBuf::from(named);
        if with_extension.is_file() {
            return Some(with_extension);
        }
    }

    None
}

/// The suffixes `PATHEXT` names, or the documented default when it is unset.
#[cfg(windows)]
pub(super) fn path_extensions() -> Vec<String> {
    const DEFAULT: &str = ".COM;.EXE;.BAT;.CMD";

    std::env::var("PATHEXT")
        .unwrap_or_else(|_| DEFAULT.to_owned())
        .split(';')
        .map(str::trim)
        .filter(|extension| !extension.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `cmd.exe` builtins, which are not on PATH. Only used for a better error.
#[cfg(windows)]
pub(super) fn is_cmd_builtin(program: &str) -> bool {
    const BUILTINS: &[&str] = &[
        "assoc", "break", "call", "cd", "chdir", "cls", "color", "copy", "date", "del", "dir",
        "echo", "endlocal", "erase", "exit", "for", "ftype", "goto", "if", "md", "mkdir", "move",
        "path", "pause", "popd", "prompt", "pushd", "rd", "rem", "ren", "rename", "rmdir", "set",
        "setlocal", "shift", "start", "time", "title", "type", "ver", "verify", "vol",
    ];

    let name = program.trim().to_lowercase();
    let stem = name
        .rsplit_once('.')
        .map_or(name.as_str(), |(stem, _)| stem);
    BUILTINS.contains(&stem)
}
