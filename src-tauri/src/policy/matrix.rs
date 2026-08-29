//! The decision table of PLAN 3.
//!
//! Six rows over five tools, written out once. Every branch here answers one
//! question — *auto, ask, or refuse* — and nothing else in the runtime is
//! allowed to answer it, which is the point of the table being a single
//! `match` rather than a check inside each tool.
//!
//! Reading order inside each tool matters and is deliberate:
//!
//! 1. **Hard denials first** (PLAN 3.2). A path that will not resolve, a link
//!    that escapes while pretending not to, a write onto a socket, a program
//!    that is this application — none of these could be meaningfully approved,
//!    so offering an approval would be theatre.
//! 2. **Then containment.** Outside the workspace is always an ask, always
//!    without a session grant, because "the rest of this session" is not a
//!    scope a user can picture for the whole filesystem.
//! 3. **Then the specific rows** — sensitive names, size, `.git/`, new versus
//!    overwrite — which change the wording and the badge.
//!
//! The badge is advisory (PLAN 3.3): a `risk` of `high` never blocks anything
//! and never appears in a condition. What actually gates is the ask itself and
//! whether a grant is offered.

use std::fs;
use std::path::{Component, Path};

use super::path::{self, Resolved};
use super::{
    tool, ApprovalDetail, AskRequest, Decision, Grant, PolicyCtx, ResolvedCall, Risk,
    ScreenGeometry, ToolCall,
};
use crate::error::ErrorCode;

/// Above this, a contained read stops being routine and is asked about.
///
/// The number is a judgement about attention, not about safety: a megabyte of
/// file is more than a user can skim in an approval dialog, and it is enough
/// output to matter in a transcript.
const READ_ASK_BYTES: u64 = 1024 * 1024;

/// How much of a pending write the dialog gets to show.
const PREVIEW_BYTES: usize = 4 * 1024;

/// Names that turn an auto-allow into an ask (PLAN 3, sensitive-name
/// predicate).
///
/// `*` is allowed at one end only, which is all these patterns need. Matching
/// is case-insensitive, so every pattern here is written in lower case.
///
/// A match never blocks. It downgrades an auto-allow to an ask and raises the
/// badge — the user, not the list, decides.
const SENSITIVE: &[&str] = &[
    ".env*",
    "*.pem",
    "*.key",
    "*.p12",
    "id_rsa*",
    "id_ed25519*",
    ".npmrc",
    ".netrc",
    "credentials",
    ".aws",
    ".ssh",
    "*.kdbx",
];

/// Applies the table to one parsed call.
///
/// `workspace` is the session's canonical root; [`decide`](super::decide) has
/// already refused the call if there is none.
pub(super) fn decide(ctx: &PolicyCtx<'_>, workspace: &Path, call: ToolCall) -> Decision {
    // `judge` uses `?` for the hard denials, which are the majority of its
    // early exits. Both arms are a decision; the split is only control flow.
    match judge(ctx, workspace, call) {
        Ok(decision) | Err(decision) => decision,
    }
}

/// The table. `Err` is a hard denial, `Ok` an auto-allow or an ask.
fn judge(ctx: &PolicyCtx<'_>, workspace: &Path, call: ToolCall) -> Result<Decision, Decision> {
    match call {
        ToolCall::FsList { path, max_entries } => {
            let target = resolve(workspace, &path)?;
            let shown = target.path.display().to_string();
            let call = ResolvedCall::FsList {
                path: target.path.clone(),
                max_entries,
            };

            if target.inside {
                return Ok(Decision::Auto {
                    call,
                    reason: "a read-only listing inside the workspace",
                });
            }

            Ok(ask(
                call,
                AskRequest {
                    tool: tool::FS_LIST,
                    risk: Risk::Medium,
                    title: "List folder",
                    summary: shown.clone(),
                    detail: ApprovalDetail::FsList { path: shown },
                    grant: None,
                    scope_label: scope_label(None),
                    reason: "this folder is outside the workspace".to_owned(),
                },
            ))
        }

        ToolCall::FsRead {
            path,
            offset,
            limit,
        } => {
            let target = resolve(workspace, &path)?;
            let bytes = fs::metadata(&target.path).ok().map(|meta| meta.len());
            let shown = target.path.display().to_string();
            let label = relative_label(workspace, &target);
            let detail = ApprovalDetail::FsRead { path: shown, bytes };
            let call = ResolvedCall::FsRead {
                path: target.path.clone(),
                offset,
                limit,
            };

            if !target.inside {
                return Ok(ask(
                    call,
                    AskRequest {
                        tool: tool::FS_READ,
                        risk: Risk::High,
                        title: "Read file",
                        summary: summarize(&label, bytes),
                        detail,
                        grant: None,
                        scope_label: scope_label(None),
                        reason: "this file is outside the workspace".to_owned(),
                    },
                ));
            }

            // Checked before the size row on purpose: a large secret is a
            // secret first. The sensitive row offers no grant, the size row
            // does, and the safer of the two has to win.
            if is_sensitive(workspace, &target) {
                return Ok(ask(
                    call,
                    AskRequest {
                        tool: tool::FS_READ,
                        risk: Risk::High,
                        title: "Read file",
                        summary: summarize(&label, bytes),
                        detail,
                        grant: None,
                        scope_label: scope_label(None),
                        reason: "the name of this file suggests it holds a credential".to_owned(),
                    },
                ));
            }

            if bytes.is_some_and(|len| len > READ_ASK_BYTES) {
                let grant = Grant::FsReadLarge;
                return Ok(ask(
                    call,
                    AskRequest {
                        tool: tool::FS_READ,
                        risk: Risk::Low,
                        title: "Read file",
                        summary: summarize(&label, bytes),
                        detail,
                        scope_label: scope_label(Some(&grant)),
                        grant: Some(grant),
                        reason: format!("this file is larger than {}", human_bytes(READ_ASK_BYTES)),
                    },
                ));
            }

            Ok(Decision::Auto {
                call,
                reason: "an ordinary read inside the workspace",
            })
        }

        ToolCall::FsWrite {
            path,
            content,
            create_dirs,
        } => {
            let target = resolve(workspace, &path)?;
            let existing = fs::metadata(&target.path).ok();
            writable(&target, existing.as_ref(), create_dirs)?;

            let exists = existing.is_some();
            let bytes = content.len() as u64;
            let label = relative_label(workspace, &target);
            let summary = format!(
                "{label} ({}, {})",
                human_bytes(bytes),
                if exists { "overwrite" } else { "new file" }
            );
            let detail = ApprovalDetail::FsWrite {
                path: target.path.display().to_string(),
                bytes,
                exists,
                preview: preview(&content),
            };
            let call = ResolvedCall::FsWrite {
                path: target.path.clone(),
                content,
                create_dirs,
            };

            if !target.inside {
                return Ok(ask(
                    call,
                    AskRequest {
                        tool: tool::FS_WRITE,
                        risk: Risk::High,
                        title: "Write file",
                        summary,
                        detail,
                        grant: None,
                        scope_label: scope_label(None),
                        reason: "this file is outside the workspace".to_owned(),
                    },
                ));
            }

            // Git's own directory is where a workspace keeps its history. A
            // write there can rewrite what the user thinks is already saved,
            // so it is asked about every time and never granted for a session.
            if in_git_dir(workspace, &target) {
                return Ok(ask(
                    call,
                    AskRequest {
                        tool: tool::FS_WRITE,
                        risk: Risk::High,
                        title: "Write file",
                        summary,
                        detail,
                        grant: None,
                        scope_label: scope_label(None),
                        reason: "this file is inside .git/, where the repository keeps its history"
                            .to_owned(),
                    },
                ));
            }

            let grant = Grant::FsWrite;
            Ok(ask(
                call,
                AskRequest {
                    tool: tool::FS_WRITE,
                    // The sensitive-name rule raises the badge here too. It
                    // changes no gate — a contained write is asked about
                    // either way — but a user answering a prompt about
                    // `id_rsa` should be told what the name looks like.
                    risk: if is_sensitive(workspace, &target) {
                        Risk::High
                    } else {
                        Risk::Medium
                    },
                    title: "Write file",
                    summary,
                    detail,
                    scope_label: scope_label(Some(&grant)),
                    grant: Some(grant),
                    reason: if exists {
                        "this replaces a file that is already there".to_owned()
                    } else {
                        "this creates a file in the workspace".to_owned()
                    },
                },
            ))
        }

        ToolCall::ShellExec {
            program,
            args,
            cwd,
            timeout_ms,
        } => {
            let program = program.trim().to_owned();
            if program.is_empty() {
                return Err(Decision::deny(
                    ErrorCode::Denied,
                    "no program was given to run",
                ));
            }
            if is_self(ctx, &program) {
                return Err(Decision::deny(
                    ErrorCode::Denied,
                    "Aegis will not run itself as a tool",
                ));
            }

            let directory = match cwd.as_deref() {
                Some(raw) => resolve(workspace, raw)?,
                None => Resolved {
                    path: workspace.to_path_buf(),
                    inside: true,
                    looked_inside: true,
                },
            };
            if !directory.path.is_dir() {
                return Err(Decision::deny(
                    ErrorCode::PathInvalid,
                    format!(
                        "`{}` is not a folder to run a command in",
                        directory.path.display()
                    ),
                ));
            }

            let line = shell_line(&program, &args);
            let detail = ApprovalDetail::Shell {
                program: program.clone(),
                args: args.clone(),
                cwd: directory.path.display().to_string(),
                shell_line: line.clone(),
            };
            let call = ResolvedCall::ShellExec {
                program: program.clone(),
                args,
                cwd: directory.path.clone(),
                timeout_ms,
            };

            if !directory.inside {
                return Ok(ask(
                    call,
                    AskRequest {
                        tool: tool::SHELL_EXEC,
                        risk: Risk::High,
                        title: "Run shell command",
                        summary: line,
                        detail,
                        grant: None,
                        scope_label: scope_label(None),
                        reason: "the working directory is outside the workspace".to_owned(),
                    },
                ));
            }

            let grant = Grant::shell(&program);
            Ok(ask(
                call,
                AskRequest {
                    tool: tool::SHELL_EXEC,
                    risk: Risk::High,
                    title: "Run shell command",
                    summary: line,
                    detail,
                    scope_label: scope_label(Some(&grant)),
                    grant: Some(grant),
                    reason: "a command runs with your own privileges and is not sandboxed"
                        .to_owned(),
                },
            ))
        }

        ToolCall::ScreenCapture { display } => {
            let requested = display.as_deref().unwrap_or("primary");
            if !requested.eq_ignore_ascii_case("primary") {
                return Err(Decision::deny(
                    ErrorCode::ToolFailed,
                    format!("`{requested}` is not a display this build can capture"),
                ));
            }

            let geometry = ctx.screen;
            let name = geometry.map_or("the primary display", |screen| screen.display.as_str());
            let call = ResolvedCall::ScreenCapture {
                display: "primary".to_owned(),
            };

            let grant = Grant::ScreenCapture;
            Ok(ask(
                call,
                AskRequest {
                    tool: tool::SCREEN_CAPTURE,
                    risk: Risk::Medium,
                    title: "Capture the screen",
                    summary: describe_screen(name, geometry),
                    detail: ApprovalDetail::Screen {
                        display: name.to_owned(),
                        width: geometry.map_or(0, |screen| screen.width),
                        height: geometry.map_or(0, |screen| screen.height),
                    },
                    scope_label: scope_label(Some(&grant)),
                    grant: Some(grant),
                    // PLAN 5.4: this is the tool with the largest blast radius
                    // in the MVP, and the prompt should say why rather than
                    // leaving the user to work it out.
                    reason: "a capture includes every window on that display, not just Aegis"
                        .to_owned(),
                },
            ))
        }
    }
}

/// Wraps a request into an ask.
fn ask(call: ResolvedCall, request: AskRequest) -> Decision {
    Decision::Ask {
        call,
        request: Box::new(request),
    }
}

/// Resolves a path argument, turning both failure modes into hard denials.
///
/// The two are kept apart because they are different accusations. A path that
/// will not resolve is a broken argument. A path that resolves outside the
/// workspace *after* looking contained is a link escape, and PLAN 3.2 refuses
/// it without a prompt precisely because the approval dialog would have to
/// show the user a path that is not the one that would be touched.
fn resolve(workspace: &Path, raw: &str) -> Result<Resolved, Decision> {
    let resolved = path::resolve(workspace, raw).map_err(|err| {
        Decision::deny(
            ErrorCode::PathInvalid,
            format!("`{}`: {}", raw.trim(), err.reason()),
        )
    })?;

    if resolved.escaped() {
        tracing::warn!("a tool call tried to leave the workspace through a link");
        return Err(Decision::deny(
            ErrorCode::PathOutsideWorkspace,
            format!(
                "`{}` reads as a path inside the workspace, but a link takes it outside",
                raw.trim()
            ),
        ));
    }

    Ok(resolved)
}

/// The hard denials specific to writing (PLAN 3.2).
///
/// Approving any of these would be approving something that cannot happen: a
/// write onto a directory or a device does not become possible because a user
/// clicked Allow, and a missing parent with `create_dirs: false` is a call
/// that is already decided.
fn writable(
    target: &Resolved,
    existing: Option<&fs::Metadata>,
    create_dirs: bool,
) -> Result<(), Decision> {
    let shown = target.path.display();

    if let Some(meta) = existing {
        if meta.is_dir() {
            return Err(Decision::deny(
                ErrorCode::PathInvalid,
                format!("`{shown}` is a folder, not a file"),
            ));
        }
        if !meta.is_file() {
            return Err(Decision::deny(
                ErrorCode::PathInvalid,
                format!("`{shown}` is not a regular file"),
            ));
        }
    }

    let Some(parent) = target.path.parent() else {
        return Err(Decision::deny(
            ErrorCode::PathInvalid,
            format!("`{shown}` is a filesystem root, not a file"),
        ));
    };

    match fs::metadata(parent) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(Decision::deny(
            ErrorCode::PathInvalid,
            format!("`{}` is not a folder", parent.display()),
        )),
        // A missing parent is only a problem when nothing would create it.
        Err(_) if create_dirs => match nearest_existing(parent) {
            Some(ancestor) if ancestor.is_dir() => Ok(()),
            Some(ancestor) => Err(Decision::deny(
                ErrorCode::PathInvalid,
                format!("`{}` is not a folder", ancestor.display()),
            )),
            None => Ok(()),
        },
        Err(_) => Err(Decision::deny(
            ErrorCode::PathInvalid,
            format!("`{}` does not exist", parent.display()),
        )),
    }
}

/// The closest ancestor of `path` that is present on disk.
fn nearest_existing(path: &Path) -> Option<&Path> {
    path.ancestors().find(|ancestor| ancestor.exists())
}

/// Whether the program names this application's own binary.
///
/// Compared through [`Grant::shell`], so the comparison folds exactly the same
/// things the grant key folds — basename, executable suffix and case on
/// Windows — and `aegis`, `Aegis.exe` and a full path to it are one answer.
fn is_self(ctx: &PolicyCtx<'_>, program: &str) -> bool {
    ctx.self_exe
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .is_some_and(|name| Grant::shell(program) == Grant::shell(name))
}

/// Whether any segment below the workspace root is `.git`.
fn in_git_dir(workspace: &Path, target: &Resolved) -> bool {
    target
        .relative_to(workspace)
        .is_some_and(|relative| segments(&relative).any(|name| name.eq_ignore_ascii_case(".git")))
}

/// Whether any segment below the workspace root looks like a credential.
///
/// Tested against the path *relative to the workspace* rather than the whole
/// thing, which is a deliberate narrowing of PLAN 3's wording. The workspace
/// root is a folder the user chose and already approved; letting one of its
/// own ancestors — a checkout under `~/.ssh/`, a directory called
/// `credentials/` — tag every read inside it as sensitive would train the user
/// to click through the prompt that is supposed to mean something. Everything
/// below the root is still tested, segment by segment, as written.
///
/// Outside the workspace there is no relative path and this returns `false`;
/// those rows already ask every time, at `high`, with no grant on offer.
fn is_sensitive(workspace: &Path, target: &Resolved) -> bool {
    target.relative_to(workspace).is_some_and(|relative| {
        segments(&relative).any(|name| {
            let name = name.to_lowercase();
            SENSITIVE.iter().any(|pattern| glob(pattern, &name))
        })
    })
}

/// The `Normal` components of a path, as strings.
///
/// A component that is not valid Unicode is skipped rather than guessed at: no
/// pattern here could match it, and a lossy conversion would invent a name
/// that is not on disk.
fn segments(path: &Path) -> impl Iterator<Item = &str> {
    path.components().filter_map(|component| match component {
        Component::Normal(name) => name.to_str(),
        _ => None,
    })
}

/// Matches one of the [`SENSITIVE`] patterns: a literal, or a single `*` at
/// one end. `name` is already lower case.
fn glob(pattern: &str, name: &str) -> bool {
    if let Some(suffix) = pattern.strip_prefix('*') {
        name.ends_with(suffix)
    } else if let Some(prefix) = pattern.strip_suffix('*') {
        name.starts_with(prefix)
    } else {
        name == pattern
    }
}

/// How to name a path in a one-line summary: relative inside the workspace,
/// absolute outside it, because that is the difference that matters to a user
/// reading the prompt.
fn relative_label(workspace: &Path, target: &Resolved) -> String {
    target
        .relative_to(workspace)
        .filter(|relative| !relative.as_os_str().is_empty())
        .map_or_else(
            || target.path.display().to_string(),
            |relative| relative.display().to_string(),
        )
}

/// `src/main.rs (2.4 KB)`, or just the path when the size is unknown.
fn summarize(label: &str, bytes: Option<u64>) -> String {
    bytes.map_or_else(
        || label.to_owned(),
        |len| format!("{label} ({})", human_bytes(len)),
    )
}

/// `the primary display (2560 x 1440)`, or the name alone before Phase 9 can
/// measure it.
fn describe_screen(name: &str, geometry: Option<&ScreenGeometry>) -> String {
    geometry.map_or_else(
        || name.to_owned(),
        |screen| format!("{name} ({} x {})", screen.width, screen.height),
    )
}

/// The scope sentence for an approval, whether or not a grant is on offer.
fn scope_label(grant: Option<&Grant>) -> String {
    grant.map_or_else(
        || "this one call, and nothing else".to_owned(),
        Grant::scope_label,
    )
}

/// The first [`PREVIEW_BYTES`] of a pending write, cut on a character
/// boundary.
///
/// Always `Some` for this tool: `content` arrives as a JSON string and is
/// therefore valid UTF-8 by construction. The `None` PLAN describes is kept in
/// the type for a future tool that could carry bytes.
fn preview(content: &str) -> Option<String> {
    let mut end = PREVIEW_BYTES.min(content.len());
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    Some(content[..end].to_owned())
}

/// A size a person can read.
///
/// The `f64` conversions lose precision above 2^53 bytes, which is eight
/// petabytes; the number in the prompt is a rounded one decimal place either
/// way.
#[allow(clippy::cast_precision_loss)]
fn human_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    match bytes {
        0..KB => format!("{bytes} B"),
        KB..MB => format!("{:.1} KB", bytes as f64 / KB as f64),
        MB..GB => format!("{:.1} MB", bytes as f64 / MB as f64),
        _ => format!("{:.1} GB", bytes as f64 / GB as f64),
    }
}

/// A display-only rendering of a command.
///
/// Never executed and never parsed back: `shell_exec` spawns the program with
/// its argument vector directly, with no shell in between (PLAN 5.1). Quoting
/// here is for legibility, not for safety, and must never be described as the
/// latter.
///
/// Shared with [`tools::shell`](crate::tools::shell) so the line a user reads
/// in the approval dialog and the line the transcript reports afterwards are
/// produced by the same function, and cannot come to disagree about what was
/// run.
pub(crate) fn shell_line(program: &str, args: &[String]) -> String {
    std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .map(quote)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Wraps a word in quotes when leaving it bare would misread.
fn quote(word: &str) -> String {
    if word.is_empty() {
        return "\"\"".to_owned();
    }
    if word
        .chars()
        .any(|c| c.is_whitespace() || c == '"' || c == '\'')
    {
        return format!("\"{}\"", word.replace('"', "\\\""));
    }
    word.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_patterns_match_at_the_right_end() {
        assert!(glob(".env*", ".env.local"));
        assert!(glob("*.pem", "server.pem"));
        assert!(glob("credentials", "credentials"));
        assert!(!glob("*.pem", "pem.txt"));
        assert!(!glob("id_rsa*", "my_id_rsa"));
    }

    #[test]
    fn sizes_are_readable() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.0 KB");
        assert_eq!(human_bytes(2 * 1024 * 1024), "2.0 MB");
    }

    #[test]
    fn command_lines_quote_what_would_misread() {
        assert_eq!(shell_line("git", &["status".to_owned()]), "git status");
        assert_eq!(
            shell_line(
                "git",
                &["commit".to_owned(), "-m".to_owned(), "a b".to_owned()]
            ),
            r#"git commit -m "a b""#
        );
        assert_eq!(shell_line("echo", &[String::new()]), r#"echo """#);
    }

    #[test]
    fn a_preview_never_splits_a_character() {
        let content = "é".repeat(PREVIEW_BYTES);
        let preview = preview(&content).expect("a preview is always produced");
        assert!(preview.len() <= PREVIEW_BYTES);
        assert!(content.starts_with(&preview));
    }
}
