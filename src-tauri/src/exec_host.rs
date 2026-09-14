//! The execution host: which operating system `shell_exec` lands in
//! (PLAN 7.12).
//!
//! On Windows, a project may name a WSL distribution so `shell_exec` runs its
//! commands there (the repository's real toolchain) instead of through
//! `CreateProcess`. Only commands move; the runtime does the `wsl.exe`
//! wrapping.
//!
//! * **No host by default**, and never inferred from a `\\wsl$\` path.
//! * **Filesystem tools do not move**: containment stays the Windows-canonical
//!   workspace.
//! * **Never silent**: a missing distribution, `wsl.exe` or path fails before
//!   spawning with [`ErrorCode::ExecHost`], never falling back to Windows.
//!
//! [`ErrorCode::ExecHost`]: crate::error::ErrorCode::ExecHost

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Where a project's commands run; stored as `Option<ExecHost>`, `None` being
/// this computer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum ExecHost {
    /// A WSL distribution on this machine, by the name `wsl.exe -l` gives it.
    Wsl {
        /// The distribution's name — `Ubuntu`, `Debian`, `Ubuntu-24.04`.
        distro: String,
    },
}

/// One choice on the picker, where "this computer" is a row of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum ExecHostOption {
    /// This process, on this operating system. Always offered.
    Host,
    /// A distribution that is installed right now.
    Wsl {
        /// The distribution's name.
        distro: String,
    },
}

/// Where one command will actually land, once policy has resolved it.
///
/// Built by policy and used by both the dialog and the tool, so what the user
/// reads is what runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct ExecTarget {
    /// The distribution the command runs in.
    pub distro: String,
    /// The working directory, as a path inside that distribution.
    pub cwd: String,
}

/// Whether this build can offer a WSL host at all.
///
/// Non-Windows builds refuse a host rather than ignore it.
pub const fn supported() -> bool {
    cfg!(windows)
}

// ---------------------------------------------------------------------------
// What is installed
// ---------------------------------------------------------------------------

/// The distributions `wsl.exe -l -q` can see, plus this computer.
///
/// Queried each time, never cached.
pub async fn options() -> Vec<ExecHostOption> {
    let mut out = vec![ExecHostOption::Host];
    for distro in installed().await {
        out.push(ExecHostOption::Wsl { distro });
    }
    out
}

/// The names `wsl.exe -l -q` prints, in its own order.
///
/// Empty without WSL or when `wsl.exe` answers nothing.
#[cfg(windows)]
pub async fn installed() -> Vec<String> {
    let Some(wsl) = wsl_exe() else {
        return Vec::new();
    };

    let mut command = tokio::process::Command::new(&wsl);
    command
        .args(["-l", "-q"])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        // Or listing distributions flashes a console window (PLAN 5.1).
        .creation_flags(crate::tools::shell::CREATE_NO_WINDOW);

    let listed = match command.output().await {
        Ok(listed) => listed,
        Err(err) => {
            tracing::warn!(%err, "wsl.exe would not list its distributions");
            return Vec::new();
        }
    };

    // `wsl.exe` writes its own output as UTF-16LE, not as the code page and not
    // as UTF-8 — the one place in this runtime where that is true, and the
    // reason this does not go through the shell tool's decoder.
    utf16_lines(&listed.stdout)
}

/// The names `wsl.exe -l -q` prints. Never any, off Windows.
#[cfg(not(windows))]
pub async fn installed() -> Vec<String> {
    Vec::new()
}

/// One line of whatever `wsl.exe` said about itself.
///
/// `wsl.exe`'s own messages are UTF-16LE, the program's output is not; NUL
/// bytes (never valid UTF-8 text) tell them apart.
pub(crate) fn message(bytes: &[u8]) -> String {
    let nuls = bytes.iter().skip(1).step_by(2).filter(|b| **b == 0).count();
    let looks_wide = bytes.len() >= 4 && bytes.len() % 2 == 0 && nuls * 2 > bytes.len() / 2;

    let text = if looks_wide {
        utf16_lines(bytes).join(" ")
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    };

    // One line: this is appended to a sentence in an envelope, not printed to
    // a terminal.
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Decodes the UTF-16LE lines `wsl.exe` writes to a pipe.
///
/// Trailing `\r`, the empty last line and any stray NUL are dropped, so the
/// result is the names and nothing else.
fn utf16_lines(bytes: &[u8]) -> Vec<String> {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();

    String::from_utf16_lossy(&units)
        .lines()
        .map(|line| line.trim_matches(|ch: char| ch.is_whitespace() || ch == '\u{0}'))
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Where `wsl.exe` is, when it is anywhere.
///
/// PATH first, then `%SystemRoot%\System32`, since a GUI app's PATH may differ.
#[cfg(windows)]
pub fn wsl_exe() -> Option<PathBuf> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if let Ok(found) = crate::tools::shell::resolve("wsl.exe", &cwd) {
        return Some(found);
    }

    let fallback = PathBuf::from(std::env::var_os("SystemRoot")?)
        .join("System32")
        .join("wsl.exe");
    fallback.is_file().then_some(fallback)
}

/// Where `wsl.exe` is. Nowhere, off Windows.
#[cfg(not(windows))]
pub fn wsl_exe() -> Option<PathBuf> {
    None
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// A path in its ordinary Windows spelling: verbatim prefixes (`\\?\UNC\…`)
/// removed, separators normalized.
fn plain(text: &str) -> String {
    text.strip_prefix(r"\\?\UNC\")
        .map(|rest| format!(r"\\{rest}"))
        .unwrap_or_else(|| {
            text.strip_prefix(r"\\?\")
                .unwrap_or(text)
                .replace('/', r"\")
        })
}

/// The distribution whose filesystem this path is in, when it is in one.
///
/// The inverse of [`linux_path`], used only to point the picker at the right
/// row — never to set the host (PLAN 7.12). `None` for non-WSL paths.
pub fn distro_of(path: &Path) -> Option<String> {
    let rest = plain(path.to_str()?);
    let rest = rest.strip_prefix(r"\\")?;

    let mut parts = rest.splitn(3, '\\');
    let share = parts.next()?;
    if !share.eq_ignore_ascii_case("wsl$") && !share.eq_ignore_ascii_case("wsl.localhost") {
        return None;
    }

    // `\\wsl$\` alone names no distribution, and neither does a trailing
    // separator with nothing after it.
    let named = parts.next().unwrap_or_default();
    (!named.is_empty()).then(|| named.to_owned())
}

/// The same folder as the distribution spells it, like `wslpath -a -u`:
///
/// * `\\wsl$\Ubuntu\home\p\proj` (or `\\wsl.localhost\…`) → `/home/p/proj`;
///   another distribution's share is refused;
/// * `C:\work\proj` → `/mnt/c/work/proj` (assumes the default automount root;
///   the caller probes the directory before spawning).
///
/// `Err` is a sentence for the approval dialog.
pub fn linux_path(distro: &str, path: &Path) -> Result<String, String> {
    let Some(text) = path.to_str() else {
        return Err(format!(
            "`{}` is not a path that can be spelled for `{distro}`",
            path.display()
        ));
    };

    let text = plain(text);

    if let Some(rest) = text.strip_prefix(r"\\") {
        let mut parts = rest.splitn(3, '\\');
        let share = parts.next().unwrap_or_default();
        let named = parts.next().unwrap_or_default();
        let inside = parts.next().unwrap_or_default();

        if !share.eq_ignore_ascii_case("wsl$") && !share.eq_ignore_ascii_case("wsl.localhost") {
            return Err(format!(
                "`{text}` is a network share; `{distro}` has no path for it"
            ));
        }
        if !named.eq_ignore_ascii_case(distro) {
            return Err(format!(
                "`{text}` is inside the `{named}` distribution, and commands for this project run \
                 in `{distro}`"
            ));
        }
        return Ok(joined(inside));
    }

    // `C:\work` — a drive letter, a colon, and the rest.
    let mut chars = text.chars();
    let drive = chars.next().unwrap_or_default();
    if drive.is_ascii_alphabetic() && chars.next() == Some(':') {
        let rest = chars.as_str().trim_start_matches('\\');
        let mut out = format!("/mnt/{}", drive.to_ascii_lowercase());
        if !rest.is_empty() {
            out.push('/');
            out.push_str(&joined(rest)[1..]);
        }
        return Ok(out);
    }

    Err(format!(
        "`{text}` is not an absolute Windows path, so `{distro}` has no path for it"
    ))
}

/// The tail of a Windows path as an absolute Linux one.
fn joined(rest: &str) -> String {
    let mut out = String::from("/");
    out.push_str(
        &rest
            .split('\\')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("/"),
    );
    out
}

// ---------------------------------------------------------------------------
// What the model is told
// ---------------------------------------------------------------------------

/// The paragraph the system message carries when a project has a host.
///
/// Saves the model rounds spent on `pnpm.cmd` or `C:\` arguments.
pub fn prompt_block(host: &ExecHost, workspace: Option<&Path>) -> String {
    let ExecHost::Wsl { distro } = host;

    let mut block = format!(
        "Commands run in `{distro}`, a Linux distribution on this machine, and not on Windows. \
         `shell_exec` looks its program up on that distribution's PATH and runs it there, as that \
         distribution's own user: `pnpm` is the Linux binary, not `pnpm.cmd`."
    );

    if let Some(root) = workspace.and_then(|root| linux_path(distro, root).ok()) {
        block.push_str(&format!(
            " The workspace is `{root}` from inside it, which is the path to use in a command's \
             arguments."
        ));
    }

    block.push_str(
        " The file tools are unaffected: `fs_read`, `fs_write` and `fs_list` still take the \
         Windows paths above.",
    );
    block
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The picker's half of PLAN 7.12: which distribution a folder is in, for
    /// the row that should be easy to find. Never the host itself.
    #[test]
    fn a_unc_path_names_the_distribution_it_is_inside() {
        for spelling in [
            r"\\wsl$\Ubuntu\home\p\proj",
            r"\\wsl.localhost\Ubuntu\home\p\proj",
            r"\\?\UNC\wsl.localhost\Ubuntu\home\p\proj",
            r"\\?\UNC\wsl$\Ubuntu\home\p\proj",
            r"\\wsl$\Ubuntu",
        ] {
            assert_eq!(
                distro_of(Path::new(spelling)).as_deref(),
                Some("Ubuntu"),
                "{spelling}"
            );
        }
    }

    /// A drive path is in no distribution, even though WSL can reach it.
    #[test]
    fn an_ordinary_path_names_none() {
        for spelling in [
            r"C:\work\proj",
            r"\\?\C:\work\proj",
            r"\\server\share\proj",
            r"/home/p/proj",
            r"\\wsl$",
        ] {
            assert_eq!(distro_of(Path::new(spelling)), None, "{spelling}");
        }
    }

    /// A workspace that lives in the distribution: the share name is the
    /// distribution, and what follows it is already the Linux path.
    #[test]
    fn a_unc_path_into_the_distro_loses_the_share() {
        assert_eq!(
            linux_path("Ubuntu", Path::new(r"\\wsl$\Ubuntu\home\pierre\proj")),
            Ok("/home/pierre/proj".to_owned())
        );
        assert_eq!(
            linux_path("Ubuntu", Path::new(r"\\wsl.localhost\Ubuntu\srv")),
            Ok("/srv".to_owned())
        );
        assert_eq!(
            linux_path("Ubuntu", Path::new(r"\\wsl$\ubuntu\home")),
            Ok("/home".to_owned()),
            "Windows does not fold case on a share name and neither does this"
        );
    }

    /// A workspace on a Windows volume: WSL automounts it under `/mnt`.
    #[test]
    fn a_drive_becomes_a_mount() {
        assert_eq!(
            linux_path("Ubuntu", Path::new(r"C:\work\proj")),
            Ok("/mnt/c/work/proj".to_owned())
        );
        assert_eq!(
            linux_path("Ubuntu", Path::new(r"D:\")),
            Ok("/mnt/d".to_owned())
        );
        assert_eq!(
            linux_path("Ubuntu", Path::new(r"C:\a b\c")),
            Ok("/mnt/c/a b/c".to_owned()),
            "a space is a character, not a separator"
        );
    }

    /// The one translation that must never quietly succeed: a folder in
    /// another distribution is not a folder this one can reach, and running the
    /// command in `/` instead would be running it in the wrong place.
    #[test]
    fn another_distros_folder_is_refused_rather_than_translated() {
        let refused = linux_path("Ubuntu", Path::new(r"\\wsl$\Debian\home\p"))
            .expect_err("Ubuntu cannot see Debian's filesystem");

        assert!(refused.contains("Debian"), "{refused}");
        assert!(refused.contains("Ubuntu"), "{refused}");
    }

    #[test]
    fn a_share_and_a_relative_path_are_both_refused() {
        assert!(linux_path("Ubuntu", Path::new(r"\\server\share\x")).is_err());
        assert!(linux_path("Ubuntu", Path::new(r"work\proj")).is_err());
        assert!(linux_path("Ubuntu", Path::new("/home/p")).is_err());
    }

    /// Verbatim prefixes are what `fs::canonicalize` leaves behind. Nothing in
    /// this runtime should hand one over — every workspace goes through
    /// `dunce` — but understanding one is cheaper than a mystery refusal.
    #[test]
    fn a_verbatim_prefix_is_understood_rather_than_refused() {
        assert_eq!(
            linux_path("Ubuntu", Path::new(r"\\?\C:\work")),
            Ok("/mnt/c/work".to_owned())
        );
        assert_eq!(
            linux_path("Ubuntu", Path::new(r"\\?\UNC\wsl$\Ubuntu\home")),
            Ok("/home".to_owned())
        );
    }

    #[test]
    fn the_list_is_decoded_from_utf16() {
        // Exactly what `wsl.exe -l -q` writes: UTF-16LE, CRLF, no BOM.
        let mut bytes = Vec::new();
        for unit in "Ubuntu\r\ndocker-desktop\r\n".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }

        assert_eq!(utf16_lines(&bytes), vec!["Ubuntu", "docker-desktop"]);
        assert!(utf16_lines(&[]).is_empty());
    }

    /// The model is told which distribution, and where the workspace is from
    /// inside it — the two facts it would otherwise spend a round discovering.
    #[test]
    fn the_prompt_block_names_the_distro_and_the_linux_workspace() {
        let host = ExecHost::Wsl {
            distro: "Ubuntu".to_owned(),
        };
        let block = prompt_block(&host, Some(Path::new(r"\\wsl$\Ubuntu\home\p\proj")));

        assert!(block.contains("Ubuntu"), "{block}");
        assert!(block.contains("/home/p/proj"), "{block}");
        assert!(block.contains("fs_read"), "{block}");
    }

    /// A workspace whose folder has gone still gets the sentence that matters;
    /// it just cannot name a path that is not there.
    #[test]
    fn the_prompt_block_survives_a_workspace_it_cannot_spell() {
        let host = ExecHost::Wsl {
            distro: "Ubuntu".to_owned(),
        };
        let block = prompt_block(&host, None);

        assert!(block.contains("Ubuntu"), "{block}");
        assert!(!block.contains("workspace is `"), "{block}");
    }
}
