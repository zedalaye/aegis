//! Open a workspace path in the OS file manager (PLAN 7.10).
//!
//! A command, not a model tool; the WebView has no opener, `fs:` or `shell:`
//! permission.
//!
//! * [`target`] resolves the argument inside the workspace (empty is the root).
//! * [`open`] hands it to the file manager, selecting a file when possible.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::error::{AppError, AppResult};
use crate::policy::path;

/// The path the file manager should open, or why it must not.
///
/// `workspace` must already be canonical — it comes from
/// [`canonical_workspace`](crate::store::canonical_workspace), the same door
/// every other workspace path walks through.
pub fn target(workspace: &Path, path: Option<&str>) -> AppResult<PathBuf> {
    let Some(raw) = path.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(workspace.to_path_buf());
    };

    let resolved = path::resolve(workspace, raw).map_err(|err| AppError::RevealPath {
        path: raw.to_owned(),
        reason: err.reason().to_owned(),
    })?;

    // A symlink escape is the interesting half of "not inside": the argument
    // read as contained and resolved elsewhere. Folding it into `!inside`
    // would still refuse, but the two cases are the same decision for this
    // command — neither is something the window is allowed to open.
    if !resolved.inside || resolved.escaped() {
        return Err(AppError::RevealOutside {
            path: raw.to_owned(),
        });
    }

    Ok(resolved.path)
}

/// Opens `path` in the OS file manager.
///
/// Fire-and-forget: Explorer, Finder and `xdg-open` outlive the spawn, and
/// Explorer on Windows in particular exits with a non-zero code on a success.
/// Waiting on them would hang the command on a window that is already up.
pub fn open(path: &Path) -> AppResult<()> {
    if !path.exists() {
        return Err(AppError::RevealPath {
            path: path.display().to_string(),
            reason: "it is not there any more".to_owned(),
        });
    }

    spawn_file_manager(path).map_err(|error| {
        tracing::error!(
            error = %error,
            path = %path.display(),
            "file manager would not start"
        );
        AppError::Internal {
            what: "could not open the file manager",
        }
    })?;
    Ok(())
}

fn spawn_file_manager(path: &Path) -> std::io::Result<()> {
    let mut command = file_manager_command(path);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.spawn()?;
    Ok(())
}

fn file_manager_command(path: &Path) -> Command {
    #[cfg(windows)]
    {
        let mut command = Command::new("explorer");
        if path.is_file() {
            // `/select,<path>` is one argument. Splitting it lets Explorer
            // treat `/select,` as a folder name and ignore the path.
            let mut arg = std::ffi::OsString::from("/select,");
            arg.push(path);
            command.arg(arg);
        } else {
            command.arg(path);
        }
        command
    }

    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("open");
        if path.is_file() {
            command.arg("-R");
        }
        command.arg(path);
        command
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    {
        // `xdg-open` on a file launches the default handler, which is an
        // editor more often than a file manager. The parent directory is the
        // honest best-effort on Linux (PLAN 7.10).
        let mut command = Command::new("xdg-open");
        if path.is_file() {
            command.arg(path.parent().unwrap_or(path));
        } else {
            command.arg(path);
        }
        command
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn temp() -> (TempDir, PathBuf) {
        let dir = TempDir::new().expect("temp dir");
        let path = dunce::canonicalize(dir.path()).expect("canonical temp dir");
        (dir, path)
    }

    #[test]
    fn no_path_is_the_workspace_root() {
        let (_guard, ws) = temp();

        assert_eq!(target(&ws, None).expect("root"), ws);
        assert_eq!(target(&ws, Some("")).expect("empty"), ws);
        assert_eq!(target(&ws, Some("   ")).expect("blank"), ws);
    }

    #[test]
    fn a_contained_file_resolves() {
        let (_guard, ws) = temp();
        fs::create_dir(ws.join("src")).expect("mkdir");
        fs::write(ws.join("src/main.rs"), "fn main() {}").expect("write");

        let got = target(&ws, Some("src/main.rs")).expect("inside");
        assert_eq!(got, ws.join("src").join("main.rs"));
    }

    #[test]
    fn a_relative_climb_is_refused() {
        let (_guard, ws) = temp();

        match target(&ws, Some("../outside")) {
            Err(AppError::RevealOutside { path }) => assert_eq!(path, "../outside"),
            other => panic!("expected RevealOutside, got {other:?}"),
        }
    }

    #[test]
    fn an_absolute_path_outside_the_workspace_is_refused() {
        let (_guard, ws) = temp();
        let elsewhere = TempDir::new().expect("other dir");
        let elsewhere = dunce::canonicalize(elsewhere.path()).expect("canonical");
        let elsewhere = elsewhere.to_string_lossy();

        match target(&ws, Some(elsewhere.as_ref())) {
            Err(AppError::RevealOutside { .. }) => {}
            other => panic!("expected RevealOutside, got {other:?}"),
        }
    }

    #[test]
    fn a_malformed_path_is_not_a_containment_miss() {
        let (_guard, ws) = temp();

        match target(&ws, Some("   \t")) {
            Ok(path) => assert_eq!(path, ws),
            other => panic!("whitespace is the root, got {other:?}"),
        }
    }

    #[test]
    fn open_refuses_a_path_that_is_gone() {
        let (_guard, ws) = temp();
        let gone = ws.join("nope");

        match open(&gone) {
            Err(AppError::RevealPath { .. }) => {}
            other => panic!("expected RevealPath, got {other:?}"),
        }
    }
}
