//! The decision table of PLAN 3: *auto, ask or refuse* is answered here and
//! nowhere else.
//!
//! Order inside each tool: hard denials first (PLAN 3.2), then containment
//! (outside the workspace always asks, with no grant), then the specific rows,
//! which change the wording and the badge. The badge is advisory and never a
//! condition.

use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path};

use crate::store::connectors;

mod files;
mod jev;
mod shell;

use super::grants;
use super::path::{self, Resolved};
use super::{
    tool, ApprovalDetail, AskRequest, Decision, Grant, HandoffRow, PolicyCtx, ResolvedCall, Risk,
    ScreenGeometry, ToolCall,
};
use crate::agent::decision::eval::{self, EvalWrite, Loaded};
use crate::error::ErrorCode;
use crate::exec_host::{self, ExecHost, ExecTarget};
use crate::handoff::{self, bus};
use crate::skills;
use crate::workspace;
use crate::world;

/// Above this, a contained read is asked about. A judgement about attention,
/// not safety.
const READ_ASK_BYTES: u64 = 1024 * 1024;

/// How much of a pending write the dialog gets to show.
const PREVIEW_BYTES: usize = 4 * 1024;

/// Names that turn an auto-allow into an ask and raise the badge (PLAN 3). Lower
/// case; `*` at one end only. A match never blocks.
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
        ToolCall::FsList { path, max_entries } => files::list(workspace, path, max_entries),

        ToolCall::FsRead {
            path,
            offset,
            limit,
        } => files::read(workspace, path, offset, limit),

        ToolCall::FsWrite {
            path,
            content,
            create_dirs,
        } => files::write(ctx, workspace, path, content, create_dirs),

        ToolCall::ShellExec {
            program,
            args,
            cwd,
            timeout_ms,
        } => shell::exec(ctx, workspace, program, args, cwd, timeout_ms),

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
                    tool: tool::SCREEN_CAPTURE.to_owned(),
                    risk: Risk::Medium,
                    title: "Capture the screen",
                    summary: describe_screen(name, geometry),
                    detail: ApprovalDetail::Screen {
                        display: name.to_owned(),
                        width: geometry.map_or(0, |screen| screen.width),
                        height: geometry.map_or(0, |screen| screen.height),
                        logical_width: geometry.map_or(0, |screen| screen.logical_width),
                        logical_height: geometry.map_or(0, |screen| screen.logical_height),
                    },
                    scope_label: scope_label(Some(&grant)),
                    grant: Some(grant),
                    // The largest blast radius of any tool (PLAN 5.4): say why.
                    reason: "a capture includes every window on that display, not just Aegis"
                        .to_owned(),
                },
            ))
        }

        // Phase 13: both auto. Neither reaches past this process, and every
        // step a runbook asks for is judged here on its own. The skill
        // allow-list was checked in `decide_call`.
        ToolCall::SkillRun { name } => Ok(Decision::Auto {
            call: ResolvedCall::SkillRun { name },
            reason: "loading a runbook this identity was granted",
        }),

        ToolCall::SkillReturn { report } => {
            // A return claims files, and only workspace files can be claimed.
            let report = contained(workspace, *report)?;
            Ok(Decision::Auto {
                call: ResolvedCall::SkillReturn {
                    report: Box::new(report),
                },
                reason: "recording the result of a skill run",
            })
        }

        // Phase 14. A memory reaches every later turn, so writing one asks,
        // like `fs_write`.
        ToolCall::MemoryWrite { kind, text, source } => {
            let trimmed = text.trim();
            // The store refuses this too; catching it here keeps a dialog from
            // asking to remember nothing.
            if trimmed.is_empty() {
                return Err(Decision::deny(
                    ErrorCode::ToolFailed,
                    "there is nothing to remember. Say what is worth remembering, in one \
                     sentence"
                        .to_owned(),
                ));
            }

            let source = source
                .map(|source| source.trim().to_owned())
                .filter(|source| !source.is_empty());
            let grant = Grant::MemoryWrite;

            Ok(ask(
                ResolvedCall::MemoryWrite {
                    kind,
                    text: trimmed.to_owned(),
                    source: source.clone(),
                },
                AskRequest {
                    tool: tool::MEMORY_WRITE.to_owned(),
                    // Durable but reversible, and inside this application.
                    risk: Risk::Medium,
                    title: "Remember this",
                    summary: summarize_memory(kind.as_str(), trimmed),
                    detail: ApprovalDetail::Memory {
                        memory_kind: kind.as_str().to_owned(),
                        text: trimmed.to_owned(),
                        source,
                    },
                    scope_label: scope_label(Some(&grant)),
                    grant: Some(grant),
                    reason: "a memory reaches every later turn this identity takes".to_owned(),
                },
            ))
        }

        // Auto: the query cannot name an identity, so it only reads the
        // caller's own memories.
        ToolCall::MemorySearch { query } => Ok(Decision::Auto {
            call: ResolvedCall::MemorySearch {
                query: query.trim().to_owned(),
            },
            reason: "reading this identity's own memories",
        }),

        // Phase 15. Delegating asks: it is the only call that makes other
        // agents run, and who works on what is the human's decision. The
        // session grant covers the routing; each specialist's calls still ask.
        ToolCall::HandoffDelegate { plan } => {
            let briefs = &plan.briefs;
            if briefs.is_empty() {
                return Err(Decision::deny(
                    ErrorCode::ToolFailed,
                    "there are no briefs to hand out. `briefs` is what this call is for".to_owned(),
                ));
            }
            if briefs.len() > bus::FAN_OUT_MAX {
                return Err(Decision::deny(
                    ErrorCode::ToolFailed,
                    format!(
                        "{} briefs is more than the {} this can carry at once. Route the most \
                         important ones now and the rest when they come back — a fan-out this \
                         wide is a decision that has not been made yet",
                        briefs.len(),
                        bus::FAN_OUT_MAX
                    ),
                ));
            }

            // Before the dialog, so nobody approves a delegation the bus would
            // refuse. `E_TOOL_FAILED`: the brief is malformed, not forbidden.
            for one in briefs.iter().chain(plan.review.iter()) {
                if let Err(reason) = handoff::check_brief(one) {
                    return Err(Decision::deny(ErrorCode::ToolFailed, reason));
                }
            }

            let rows: Vec<HandoffRow> = briefs.iter().map(row).collect();
            let reviewer = plan.review.as_ref().map(|one| one.owner.trim().to_owned());
            let filed_in = workspace
                .join(workspace::BRIEFS_DIR)
                .is_dir()
                .then(|| format!("{}/", workspace::BRIEFS_DIR));
            let grant = Grant::HandoffDelegate;

            Ok(ask(
                ResolvedCall::HandoffDelegate { plan: plan.clone() },
                AskRequest {
                    tool: tool::HANDOFF_DELEGATE.to_owned(),
                    // It spends other identities' turns; read the owners.
                    risk: Risk::Medium,
                    title: "Hand out work",
                    summary: summarize_handoff(&rows, reviewer.as_deref()),
                    detail: ApprovalDetail::Handoff {
                        briefs: rows,
                        reviewer,
                        filed_in,
                    },
                    scope_label: scope_label(Some(&grant)),
                    grant: Some(grant),
                    reason: "this starts other identities working, each under its own allow-list"
                        .to_owned(),
                },
            ))
        }

        // Auto, like `skill_return`: it validates a report with contained
        // artefacts.
        ToolCall::HandoffReturn { report } => {
            let report = contained(workspace, *report)?;
            Ok(Decision::Auto {
                call: ResolvedCall::HandoffReturn {
                    report: Box::new(report),
                },
                reason: "returning the brief this run was given",
            })
        }

        // Phase 18: a tool this build did not write always asks (PLAN 7.2
        // row 7); nothing can be resolved in a call whose effects happen in
        // another process. `readOnlyHint` is shown, attributed, and never
        // branched on.
        ToolCall::Connector { name, args } => {
            // Nothing answers to this name — the connector stopped, or the name
            // is invented — so there is nothing to approve.
            let Some(info) = ctx.connectors.and_then(|catalog| catalog.find(&name)) else {
                let (connector, tool) =
                    connectors::split_tool_name(&name).unwrap_or((name.as_str(), name.as_str()));
                return Err(Decision::deny(
                    ErrorCode::ToolFailed,
                    format!(
                        "nothing answers to `{tool}` right now: the `{connector}` connector is \
                         not running, or does not offer it. Connectors are installed by the user \
                         in Settings — you cannot start one. Say what you needed it for"
                    ),
                ));
            };

            // The model's arguments, indented for reading; nothing is resolved.
            let arguments =
                serde_json::to_string_pretty(&args).unwrap_or_else(|_| args.to_string());
            let grant = Grant::Connector { tool: name.clone() };

            Ok(ask(
                ResolvedCall::Connector {
                    name: name.clone(),
                    args,
                },
                AskRequest {
                    tool: name.clone(),
                    risk: Risk::High,
                    title: "Call a connector",
                    summary: format!("{} · {}", info.connector_name, info.name),
                    detail: ApprovalDetail::Connector {
                        connector: info.connector.clone(),
                        connector_name: info.connector_name.clone(),
                        tool: info.name.clone(),
                        description: info.description.clone(),
                        read_only_hint: info.read_only_hint,
                        arguments,
                    },
                    scope_label: scope_label(Some(&grant)),
                    grant: Some(grant),
                    reason: "this runs inside a program Aegis did not write, and nothing here \
                             can check what it does with these arguments"
                        .to_owned(),
                },
            ))
        }

        ToolCall::JevEval { name, inputs } => jev::eval_call(workspace, name, inputs),

        ToolCall::JevAsk { state, questions } => jev::ask_call(ctx, state, questions),
    }
}

/// The content of a pending write, for recognizing what it is.
fn call_content(call: &ResolvedCall) -> &str {
    match call {
        ResolvedCall::FsWrite { content, .. } => content,
        _ => "",
    }
}

/// One line naming a memory, for the dialog header; the dialog shows the rest.
fn summarize_memory(kind: &str, text: &str) -> String {
    const HEAD: usize = 72;

    if text.chars().count() <= HEAD {
        return format!("{kind}: {text}");
    }
    let head: String = text.chars().take(HEAD).collect();
    format!("{kind}: {head}…")
}

/// Resolves a return's artefact paths, refusing any outside the workspace.
/// Whether they exist is the tool's question ([`handoff::check`]).
fn contained(workspace: &Path, draft: handoff::Draft) -> Result<handoff::Report, Decision> {
    let mut artefacts = Vec::with_capacity(draft.artefacts.len());

    for shown in &draft.artefacts {
        let target = resolve(workspace, shown)?;
        if !target.inside {
            return Err(Decision::deny(
                ErrorCode::PathOutsideWorkspace,
                format!(
                    "`{shown}` is outside the workspace, so it is not an artefact of this run. \
                     Name a path inside it"
                ),
            ));
        }
        artefacts.push(handoff::Artefact {
            shown: shown.clone(),
            path: target.path,
        });
    }

    Ok(handoff::Report {
        status: draft.status,
        summary: draft.summary,
        artefacts,
        evidence: draft.evidence,
        open_questions: draft.open_questions,
        next_owner: draft.next_owner,
    })
}

/// One brief as the dialog draws it: goal, owner, how many inputs. The rest is
/// in the brief file.
fn row(brief: &handoff::Brief) -> HandoffRow {
    HandoffRow {
        goal: brief.goal.trim().to_owned(),
        owner: brief.owner.trim().to_owned(),
        priority: brief.priority.as_str().to_owned(),
        return_format: brief.return_format.as_str().to_owned(),
        inputs: u32::try_from(brief.inputs.len()).unwrap_or(u32::MAX),
    }
}

/// One line naming a delegation's owners, for the dialog header.
fn summarize_handoff(rows: &[HandoffRow], reviewer: Option<&str>) -> String {
    // Each owner once, in first-named order.
    let mut owners: Vec<&str> = Vec::with_capacity(rows.len());
    for row in rows {
        if !owners.contains(&row.owner.as_str()) {
            owners.push(&row.owner);
        }
    }

    let mut line = match rows.len() {
        1 => format!("1 brief to {}", owners.join(", ")),
        n => format!("{n} briefs to {}", owners.join(", ")),
    };
    if let Some(reviewer) = reviewer {
        let _ = write!(line, ", reviewed by {reviewer}");
    }
    line
}

/// Wraps a request into an ask.
fn ask(call: ResolvedCall, request: AskRequest) -> Decision {
    Decision::Ask {
        call,
        request: Box::new(request),
    }
}

/// Resolves a path argument. An unresolvable path and a link escape are both
/// hard denials (PLAN 3.2): no dialog could honestly show the path a link
/// would really touch.
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

/// Hard denials specific to writing (PLAN 3.2): targets no approval could make
/// writable.
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

/// Whether the target is inside the workspace's constitution.
///
/// First segment only ([`world::in_world`]), so `src/world/` does not count.
fn in_world_dir(workspace: &Path, target: &Resolved) -> bool {
    target
        .relative_to(workspace)
        .is_some_and(|relative| world::in_world(&relative))
}

/// Whether any segment below the workspace root is `.git`.
fn in_git_dir(workspace: &Path, target: &Resolved) -> bool {
    target
        .relative_to(workspace)
        .is_some_and(|relative| segments(&relative).any(|name| name.eq_ignore_ascii_case(".git")))
}

/// Whether any segment below the workspace root looks like a credential.
///
/// Only segments below the workspace root are tested, so a workspace under a
/// sensitive-looking ancestor does not flag every read. Outside the workspace
/// this is `false`; those rows already ask at `high`.
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

/// `the primary display (2560 x 1440)`, or the name alone on a machine whose
/// window server would not describe its displays.
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
/// Never executed or parsed (PLAN 5.1): quoting is for legibility, not safety.
/// Shared with [`tools::shell`](crate::tools::shell) so dialog and transcript
/// agree.
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
    use super::shell::{bare_repository_on_the_way, git_line_not_grantable};
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

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn read_only_git_lines_are_grantable() {
        for line in [
            &[][..],
            &["status"],
            &["--no-pager", "log", "--oneline"],
            &["diff", "HEAD", "--output-indicator-new=+"],
            &["show", "HEAD:src/main.rs"],
            &["branch", "--show-current"],
            &["branch", "-a", "-v"],
        ] {
            assert_eq!(git_line_not_grantable(&args(line)), None, "{line:?}");
        }
    }

    #[test]
    fn a_git_line_that_is_not_read_only_is_not_grantable() {
        for line in [
            &["checkout", "--", "a"][..],
            &["push", "origin", "HEAD"],
            &["config", "core.fsmonitor", "calc"],
            &["st"],
            &["-c", "core.fsmonitor=calc", "status"],
            &["-ccore.pager=calc", "log"],
            &["-C", "..", "status"],
            &["--git-dir=elsewhere", "log"],
            &["--exec-path=elsewhere", "status"],
            &["--", "status"],
            &["diff", "--output=../out.txt"],
            &["diff", "--output", "../out.txt"],
            &["diff", "--no-index", "a", "b"],
            &["blame", "--contents", "/etc/passwd", "x"],
            &["log", "-p", "--ext-diff"],
            &["branch", "-D", "main"],
            &["branch", "new-branch"],
        ] {
            assert!(git_line_not_grantable(&args(line)).is_some(), "{line:?}");
        }
    }

    #[test]
    fn a_folder_laid_out_like_a_bare_repository_is_found_on_the_way_up() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let root = dunce::canonicalize(dir.path()).expect("canonical");
        let planted = root.join("planted");
        fs::create_dir_all(planted.join("objects")).expect("mkdir");
        fs::create_dir_all(planted.join("refs").join("heads")).expect("mkdir");
        fs::write(planted.join("HEAD"), "ref: refs/heads/main\n").expect("write");

        assert!(bare_repository_on_the_way(&planted.join("refs"), &root));
        assert!(!bare_repository_on_the_way(&root, &root));

        fs::create_dir(planted.join("refs").join(".git")).expect("mkdir");
        assert!(
            !bare_repository_on_the_way(&planted.join("refs"), &root),
            "a .git on the way is where git stops looking"
        );
    }
}
