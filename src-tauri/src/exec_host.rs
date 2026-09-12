//! The execution host: which operating system `shell_exec` lands in
//! (PLAN 7.12).
//!
//! The UI host and the tool host are not the same OS on a Windows operator
//! whose software projects live in a WSL distribution. Opening the code folder
//! is already the workspace — `fs_*` sees those files over UNC (`\\wsl$\…`), or
//! under `C:\` mounted at `/mnt/c` — but `shell_exec` spawns through
//! `CreateProcess`, which is the *Windows* toolchain. It is not the `git`, the
//! `docker` or the test runner the repository is actually built with.
//!
//! So a project may name an execution host, and exactly one thing changes: a
//! command is run inside that distribution instead of on Windows. Not a second
//! Aegis, not a second agent loop, not a Linux VM this process manages, and not
//! a tool the model calls — `wsl.exe` is never a `program` anybody asks for.
//! Wrapping is the runtime's, the same way launching a `.cmd` shim through
//! `cmd.exe` is the runtime's.
//!
//! Three rules hold the shape:
//!
//! * **A project has no host by default.** Picking a folder is not consent, and
//!   auto-detecting WSL from a `\\wsl$\` path would make it one. A finance or
//!   watch workspace never gets a distribution.
//! * **The filesystem tools do not move.** Containment is still the
//!   Windows-canonical workspace, `fs_read` of a file in that folder still goes
//!   through Windows, and a capture is still *this* display. There is no second
//!   filesystem here — only a second way to spell the same one.
//! * **Silence is forbidden.** A missing distribution, a `wsl.exe` that is not
//!   there, a working directory the distribution cannot see: each fails before
//!   anything is spawned, with [`ErrorCode::ExecHost`]. Quietly running the
//!   command on Windows instead would be running it on the wrong operating
//!   system, which is the one outcome worse than not running it at all.
//!
//! [`ErrorCode::ExecHost`]: crate::error::ErrorCode::ExecHost

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Where a project's commands run.
///
/// `Option<ExecHost>` is the whole type: `None` — an absent field on disk — is
/// this process, which is what every project had before this slice and what
/// every project still has until somebody says otherwise.
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

/// One choice on the picker.
///
/// Deliberately not `Option<ExecHost>`: a list of hosts has to be able to say
/// "this computer" as a row like any other, and a `null` in an array is not a
/// row somebody can click.
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
/// Built by the decision table, carried on the resolved call, drawn by the
/// approval dialog and read by the tool — one value, so the distribution and
/// the working directory the user *read* are the ones that are *run*, with no
/// second translation anywhere to disagree with the first.
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
/// A Linux or macOS build refuses one rather than ignoring it: a project record
/// carrying a host that silently does nothing is a project whose commands run
/// somewhere other than where it says they do.
pub const fn supported() -> bool {
    cfg!(windows)
}

// ---------------------------------------------------------------------------
// What is installed
// ---------------------------------------------------------------------------

/// The distributions `wsl.exe -l -q` can see, plus this computer.
///
/// Asked rather than remembered. Distributions are installed and removed by the
/// operator in a terminal, and a picker confidently listing one that was
/// uninstalled last week is worse than a picker that takes 150 ms.
pub async fn options() -> Vec<ExecHostOption> {
    let mut out = vec![ExecHostOption::Host];
    for distro in installed().await {
        out.push(ExecHostOption::Wsl { distro });
    }
    out
}

/// The names `wsl.exe -l -q` prints, in its own order.
///
/// Empty on any platform without WSL, and empty when `wsl.exe` is absent or
/// answers with nothing — in each case the picker offers this computer alone,
/// which is the truth.
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
/// `wsl.exe` writes its own diagnostics as UTF-16LE — "There is no distribution
/// with the supplied name", and the error code beside it — while the program it
/// runs writes ordinary bytes. Which of the two a pipe holds is worth deciding
/// rather than assuming, because reading UTF-16 as UTF-8 gives a string with a
/// NUL between every letter and reading the reverse gives mojibake.
///
/// The test is the NULs: text from a Western locale in UTF-16LE has one in
/// every second byte, and UTF-8 never contains one at all.
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
/// PATH first, then `%SystemRoot%\System32` — because a GUI-launched
/// application can inherit a PATH that a shell would not recognise, and the
/// System32 copy is the one Windows itself installs. The `WindowsApps` alias
/// beside it is a reparse point that resolves to the same binary.
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

/// The same folder, spelled the way the distribution spells it.
///
/// Two rules, which between them cover every path a Windows workspace can
/// actually have:
///
/// * `\\wsl$\Ubuntu\home\p\proj` and `\\wsl.localhost\Ubuntu\home\p\proj` are
///   the distribution's own filesystem seen from Windows, so the answer is what
///   is left after the share name: `/home/p/proj`. A UNC naming *another*
///   distribution is refused rather than translated — `\\wsl$\Debian\…` is not
///   a path Ubuntu has.
/// * `C:\work\proj` is a Windows volume, which WSL automounts at
///   `/mnt/c/work/proj`.
///
/// This is `wslpath -a -u`'s answer without the round trip, and the comparison
/// is deliberate: the automount root is `/mnt` unless somebody has set
/// `automount.root` in `/etc/wsl.conf`, and a translation that is wrong for
/// that reason does not go unnoticed — the caller probes the directory inside
/// the distribution before it spawns anything, so a path that is not there is a
/// refusal rather than a command that quietly ran somewhere else.
///
/// `Err` carries a sentence for the person reading the approval dialog, not a
/// parser's complaint.
/// A path in its ordinary Windows spelling, verbatim prefixes removed.
///
/// Verbatim prefixes should never reach here — every workspace is canonicalized
/// through `dunce` — but a path that arrived another way (a `canonicalize` on a
/// UNC share hands back `\\?\UNC\…`) is better understood than refused.
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
/// The inverse of [`linux_path`], and it exists for one purpose: making the
/// right row of the picker *findable*. A folder opened at
/// `\\wsl$\Ubuntu\home\…` is unambiguously inside `Ubuntu`, and a picker that
/// knows it can say so.
///
/// What it must never become is the host itself. PLAN 7.12 is explicit —
/// *auto-detecting WSL from a `\\wsl$\` path and flipping the host (picking a
/// folder is not consent)* — and the reasoning survives contact with this
/// function: the inference only catches one of the two spellings, since
/// `C:\work\proj` is just as reachable from the distribution at
/// `/mnt/c/work/proj` and just as likely to be built with its toolchain. A
/// picker that fired by itself for one and not the other would be harder to
/// understand than one that never does. So this returns a *fact about the
/// path*, and what is done with it is a click.
///
/// `None` for every ordinary path, which is nearly all of them: a drive letter,
/// a share that is not WSL's, or anything that is not a UNC path at all.
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
/// Said rather than discovered. A model that has not been told will reach for
/// `pnpm.cmd`, pass a `C:\` path as an argument, and spend a round working out
/// why `git` cannot see the repository — and the answer to each is one sentence
/// that costs nothing to include.
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

    /// An ordinary folder is in no distribution, and that includes the one a
    /// distribution can still reach: `C:\work` is mounted at `/mnt/c/work`, and
    /// calling it "inside Ubuntu" would make the picker fire for one spelling
    /// of the same situation and not the other.
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
