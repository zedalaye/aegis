//! The decision table of PLAN 3.
//!
//! Six rows over five tools, written out once, plus the two Phase 13 rows for
//! `skill_run` and `skill_return`, the two Phase 14 rows for `memory_write`
//! and `memory_search`, the two Phase 15 rows for the handoff, and the one
//! Phase 18 row that stands for every tool this build did not write. Every branch here answers one question — *auto, ask, or
//! refuse* — and nothing else in the runtime is allowed to answer it, which is
//! the point of the table being a single `match` rather than a check inside
//! each tool.
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

use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path};

use crate::store::connectors;

use super::grants;
use super::path::{self, Resolved};
use super::{
    tool, ApprovalDetail, AskRequest, Decision, Grant, HandoffRow, PolicyCtx, ResolvedCall, Risk,
    ScreenGeometry, ToolCall,
};
use crate::error::ErrorCode;
use crate::exec_host::{self, ExecHost, ExecTarget};
use crate::handoff::{self, bus};
use crate::skills;
use crate::workspace;
use crate::world;

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
                    tool: tool::FS_LIST.to_owned(),
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
                        tool: tool::FS_READ.to_owned(),
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

            // A declared source artefact of a world that is in force, still
            // byte for byte what the world was perceived from (PLAN 7.2).
            // Refused rather than asked about, and it is the one read row that
            // is: what this file said is in `world/`, so reading it again is
            // the round-trip the world exists to have paid once, and a dialog
            // offering to spend a context window on it is a dialog that teaches
            // people to click through. A source that has *moved* is not in this
            // state and falls through to the rows below — perceiving that delta
            // is the one legitimate re-perception there is.
            if let Some(declared) = world::perceived_source(workspace, &target.path) {
                return Err(Decision::deny(
                    ErrorCode::Denied,
                    format!(
                        "`{declared}` is a declared source of this world, and it has not \
                         changed since the world was perceived from it. What it says is \
                         already in `world/` — read that instead. If what is there looks \
                         wrong, say which line and stop: amending the world is a human \
                         decision"
                    ),
                ));
            }

            // Checked before the size row on purpose: a large secret is a
            // secret first. The sensitive row offers no grant, the size row
            // does, and the safer of the two has to win.
            if is_sensitive(workspace, &target) {
                return Ok(ask(
                    call,
                    AskRequest {
                        tool: tool::FS_READ.to_owned(),
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
                        tool: tool::FS_READ.to_owned(),
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
            // Whether this is the apply of a proposal (PLAN 7.13), measured
            // before the detail is built so the dialog can say so.
            let apply = target
                .relative_to(workspace)
                .and_then(|relative| skills::apply_of(workspace, &relative, &content));
            let detail = ApprovalDetail::FsWrite {
                path: target.path.display().to_string(),
                bytes,
                exists,
                preview: preview(&content),
                applies: apply.as_ref().and_then(|apply| apply.clone().ok()),
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
                        tool: tool::FS_WRITE.to_owned(),
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

            // The constitution (PLAN 7.2; `COS.md` *Work*). Which of the two
            // rows applies is a fact about the *run* and not about the
            // identity — the same identity is a Chief of Staff in one session
            // and a specialist in the next — which is why it reads
            // `ctx.delegated` rather than an allow-list.
            //
            // Inside a brief it is a **refusal**, in the same class as a
            // reviewer calling `fs_write`: specialists read the world and do
            // not write it, and an ask with a session grant on offer would be
            // the constitution asking to be overruled by whoever is quickest
            // to click. The way out of that turn is `needs_you`, not a second
            // attempt.
            if in_world_dir(workspace, &target) {
                if ctx.delegated {
                    return Err(Decision::deny(
                        ErrorCode::Denied,
                        "`world/` is this workspace's constitution, and a brief does not \
                         amend it: specialists read the world, they do not write it. If this \
                         cannot be done without changing what the thing is, that is the \
                         answer — return `needs_you` with the one sentence naming which part \
                         of the essence would have to move",
                    ));
                }

                // Outside a brief, amending the world is a cabinet act — the
                // human, or the Chief of Staff in front of them — and `COS.md`
                // asks for a *human decision*, which is what a dialog is.
                //
                // A grant is offered, and it is a narrow one of its own.
                // Founding a world is six files and amending one is rarely
                // fewer; six identical High-risk dialogs in a row is how a
                // person is taught to click through the one that mattered. What
                // the grant must not be is [`Grant::FsWrite`]: allowing writes
                // so a session could file artefacts is not agreeing to let it
                // rewrite what the project is, and the two rows never match each
                // other's key.
                //
                // Unattended, nothing is on offer. A routine has no human in it
                // to make the decision `COS.md` reserves for one, so the row
                // falls through to `decide_call`'s refusal, which says exactly
                // that. It is also unsignable in advance — `schedule::check`
                // refuses the grant when the routine is saved — so this is the
                // second of the two places, not the only one.
                let grant = (!ctx.unattended).then_some(Grant::WorldAmend);
                return Ok(ask(
                    call,
                    AskRequest {
                        tool: tool::FS_WRITE.to_owned(),
                        risk: Risk::High,
                        title: "Amend the world",
                        summary,
                        detail,
                        scope_label: scope_label(grant.as_ref()),
                        grant,
                        reason: "this is `world/`, the workspace's constitution: what the \
                                 project is, and what everything else is checked against"
                            .to_owned(),
                    },
                ));
            }

            // Applying a proposal (PLAN 7.13). The same tool, the same matrix
            // and the same audit line as any other write — what differs is
            // that this one is signed every time.
            //
            // No grant is offered, and a held `Grant::FsWrite` does not cover
            // it: allowing writes so a session could file artefacts is not
            // agreeing to make a runbook live, and the DiffPreview of *this*
            // call is the moment a person reads the seven headings. Unattended,
            // no grant means `decide_call` refuses, so a routine cannot apply.
            //
            // Inside a brief it is refused outright. A specialist that learned
            // a procedure hands back the `PROPOSAL.md` as an artefact; the
            // cabinet does not apply its own proposals on the way through.
            if let Some(apply) = apply {
                let name = apply.map_err(|reason| Decision::deny(ErrorCode::Denied, reason))?;
                if ctx.delegated {
                    return Err(Decision::deny(
                        ErrorCode::Denied,
                        format!(
                            "you are working on a brief, and a brief does not apply a proposal. \
                             Return `.aegis/skills/{name}/PROPOSAL.md` in `artefacts` and leave \
                             applying it to a person"
                        ),
                    ));
                }
                return Ok(ask(
                    call,
                    AskRequest {
                        tool: tool::FS_WRITE.to_owned(),
                        risk: Risk::High,
                        title: "Apply a skill proposal",
                        summary,
                        detail,
                        grant: None,
                        scope_label: scope_label(None),
                        reason: format!(
                            "this copies a proposal to `SKILL.md`, which makes `{name}` a runbook \
                             in this workspace's catalog. It grants it to no identity"
                        ),
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
                        tool: tool::FS_WRITE.to_owned(),
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
                    tool: tool::FS_WRITE.to_owned(),
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

            // Where it lands, before it is drawn (PLAN 7.12). A folder the
            // distribution has no path for is refused here rather than put to
            // the user: there is no answer they could give that would make the
            // command runnable, and the one thing that must not happen is for
            // it to run on Windows instead.
            let host = match ctx.exec_host {
                Some(ExecHost::Wsl { distro }) => {
                    match exec_host::linux_path(distro, &directory.path) {
                        Ok(cwd) => Some(ExecTarget {
                            distro: distro.clone(),
                            cwd,
                        }),
                        Err(reason) => return Err(Decision::deny(ErrorCode::ExecHost, reason)),
                    }
                }
                None => None,
            };

            // A program named by a path is keyed on where it is, not on what it
            // is called: `scripts\git.cmd` is a file the session may have
            // written a minute ago, and a grant on git must not run it. Resolved
            // the way `shell_exec` will resolve it — against the working
            // directory, or in the distribution's own spelling when there is a
            // host.
            let key = if grants::names_a_path(&program) {
                match &host {
                    Some(_) if program.starts_with('/') => program.clone(),
                    Some(target) => format!("{}/{program}", target.cwd.trim_end_matches('/')),
                    None => path::resolve(&directory.path, &program)
                        .map_err(|err| {
                            Decision::deny(
                                ErrorCode::PathInvalid,
                                format!("`{program}`: {}", err.reason()),
                            )
                        })?
                        .path
                        .display()
                        .to_string(),
                }
            } else {
                program.clone()
            };

            let line = shell_line(&program, &args);
            let detail = ApprovalDetail::Shell {
                program: program.clone(),
                args: args.clone(),
                cwd: directory.path.display().to_string(),
                shell_line: line.clone(),
                host: host.clone(),
            };
            let call = ResolvedCall::ShellExec {
                program: program.clone(),
                args,
                cwd: directory.path.clone(),
                host: host.map(Box::new),
                timeout_ms,
            };

            if !directory.inside {
                return Ok(ask(
                    call,
                    AskRequest {
                        tool: tool::SHELL_EXEC.to_owned(),
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

            // A session grant is the program, not the line — except git, where
            // "any arguments" is how `git status` silently covered `git
            // checkout` twice on a review.diff run (IDEAS.md § 12; PLAN 3.3).
            // The grant covers read-only git and nothing else, and the verb is
            // not enough to say so: `-c core.fsmonitor=…` makes `git status`
            // run a program, `--output` makes `git diff` write a file, and a
            // bare repository laid out in the workspace brings a configuration
            // the model wrote. Those offer no grant, so an earlier approval
            // cannot collapse them: the user sees the exact line again.
            let git_args = match &call {
                ResolvedCall::ShellExec { args, .. } => args.as_slice(),
                _ => &[],
            };
            let not_grantable = (grants::program_name(&program) == "git")
                .then(|| git_not_grantable(git_args, &directory.path, workspace))
                .flatten();
            if let Some(why) = not_grantable {
                return Ok(ask(
                    call,
                    AskRequest {
                        tool: tool::SHELL_EXEC.to_owned(),
                        risk: Risk::High,
                        title: "Run shell command",
                        summary: line,
                        detail,
                        grant: None,
                        scope_label: scope_label(None),
                        reason: format!(
                            "allowing git for the session does not cover this line: {why}"
                        ),
                    },
                ));
            }

            // The grant is keyed on the program either way — `git` is `git`
            // whichever operating system runs it, and `wsl.exe` is not a
            // program anybody was asked about. What the host changes is what
            // the user is told they are agreeing to.
            let grant = Grant::shell(&key);
            let reason = match ctx.exec_host {
                Some(ExecHost::Wsl { distro }) => format!(
                    "a command runs in `{distro}` as that distribution's own user, and is not sandboxed"
                ),
                None => {
                    "a command runs with your own privileges and is not sandboxed".to_owned()
                }
            };
            Ok(ask(
                call,
                AskRequest {
                    tool: tool::SHELL_EXEC.to_owned(),
                    risk: Risk::High,
                    title: "Run shell command",
                    summary: line,
                    detail,
                    scope_label: scope_label(Some(&grant)),
                    grant: Some(grant),
                    reason,
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
                    // PLAN 5.4: this is the tool with the largest blast radius
                    // in the MVP, and the prompt should say why rather than
                    // leaving the user to work it out.
                    reason: "a capture includes every window on that display, not just Aegis"
                        .to_owned(),
                },
            ))
        }

        // The two rows of PLAN 7.3, Phase 13. Both are `auto`, and the reason
        // is not that a new tool is assumed harmless — a new row defaults to
        // *ask* (PLAN 7.2, row 7) — but that neither of these reaches past
        // this process. `skill_run` reads a runbook the user themselves put in
        // their library or their workspace; `skill_return` writes nothing at
        // all, it validates. What a runbook then tells the model to *do* is
        // every bit as gated as it was before: each step is an ordinary call
        // through this table, so a dialog here would ask the user to approve
        // reading a file in order to be asked again about everything it says.
        //
        // The narrowing that does apply to them is the identity's skill
        // allow-list, checked in `decide_call` before this table is reached.
        ToolCall::SkillRun { name } => Ok(Decision::Auto {
            call: ResolvedCall::SkillRun { name },
            reason: "loading a runbook this identity was granted",
        }),

        ToolCall::SkillReturn { report } => {
            // Artefacts are resolved and contained like any other path the
            // model names. A return is a claim about files, and a claim about
            // a file outside the workspace is one this session has no standing
            // to make — the tool then only has to ask whether they are there.
            let report = contained(workspace, *report)?;
            Ok(Decision::Auto {
                call: ResolvedCall::SkillReturn {
                    report: Box::new(report),
                },
                reason: "recording the result of a skill run",
            })
        }

        // The two rows of PLAN 7.3, Phase 14. They are split the way the
        // filesystem rows are, and for the same reason rather than by analogy:
        // one of them reads and the other one changes something that lasts.
        //
        // A memory is not a file, but it is closer to `fs_write` than to
        // `skill_run`, because what it changes is the *next* turn's
        // instructions and every turn's after that. That is a durable,
        // invisible-at-the-time effect, and `AGENTS.md` puts those behind the
        // gate. The dialog carries the sentence itself — a memory is one
        // sentence by construction, so the user reads the whole thing rather
        // than a preview of it.
        ToolCall::MemoryWrite { kind, text, source } => {
            let trimmed = text.trim();
            // The store refuses this too, with a message written for the
            // model. Catching it here as well is not a second copy of the
            // rule: it is what keeps an approval dialog from ever asking a
            // person to approve remembering nothing.
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
                    // Durable and reversible, inside this application, touching
                    // nothing on the machine. The same badge a workspace write
                    // gets, for a change of about the same size.
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

        // Auto: it reads records this identity already holds, reaches nothing
        // outside this process, and there is no version of it a user could
        // usefully be asked about. The scoping that matters is not here at all
        // — the query cannot name an identity, so `memory_search` can only ever
        // look at the caller's own memories.
        ToolCall::MemorySearch { query } => Ok(Decision::Auto {
            call: ResolvedCall::MemorySearch {
                query: query.trim().to_owned(),
            },
            reason: "reading this identity's own memories",
        }),

        // The two rows of PLAN 7.3, Phase 15, and they are split the way the
        // memory rows are: one of them starts something, the other one reports.
        //
        // `handoff_delegate` **asks**, and it is the row where the default of
        // PLAN 7.2 row 7 — a new tool is an ask — is most obviously right. It
        // is the only call in this table that causes *other agents to run*:
        // more model requests, under other identities, with other allow-lists,
        // for as long as the timeout allows. Every step any of them then takes
        // is gated exactly as it would have been in the session the user is
        // looking at, so this dialog is not standing in for those; what it is
        // for is the decision `COS.md` gives the human — who works on what.
        //
        // A session grant is offered because a CoS that had to be re-approved
        // for every routing decision is a CoS nobody would use, and because the
        // grant covers the routing rather than the work: the specialists' own
        // writes, commands and captures still stop and ask.
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

            // Checked before the dialog, not after it: a person should never be
            // asked to approve a delegation that the bus is going to refuse.
            // The refusal is `E_TOOL_FAILED` rather than a denial about rights,
            // because nothing here is about rights — the brief is malformed,
            // and the model can write a better one.
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
                    // Not because a brief is dangerous — nothing in it runs
                    // unreviewed — but because this is the call that spends
                    // other identities' turns, and the badge is what makes a
                    // person read the owners rather than the first line.
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

        // Auto, exactly as `skill_return` is, and for the same reason: it
        // writes nothing and reaches nothing. It validates a report and hands
        // it to whoever is waiting. The artefacts are resolved and contained
        // first, because a return is a claim about files and a claim about a
        // file outside the workspace is one this run has no standing to make.
        ToolCall::HandoffReturn { report } => {
            let report = contained(workspace, *report)?;
            Ok(Decision::Auto {
                call: ResolvedCall::HandoffReturn {
                    report: Box::new(report),
                },
                reason: "returning the brief this run was given",
            })
        }

        // The Phase 18 row, and the only one in this table that covers tools
        // this build did not write. It **always asks**, and there is no branch
        // in it that could ever not.
        //
        // Every other row can auto-allow because the runtime knows what the
        // call will do: `fs_list` lists the directory policy resolved, and
        // nothing else, because that is what the function does. A connector's
        // tool is a program somebody else wrote, running as the user, with
        // arguments whose schema this process has never seen. There is no
        // predicate to write — "is this contained" has no meaning for a call
        // whose effects are in another process — so what is left is the default
        // of PLAN 7.2 row 7: a new tool is an ask.
        //
        // The server's `readOnlyHint` is read and shown, and it is shown
        // *attributed*, because it is the thing being gated describing its own
        // gate. Nothing here branches on it, and the badge does not soften for
        // it either: a call that leaves this process is `High` by the same
        // definition `shell_exec` is.
        ToolCall::Connector { name, args } => {
            // Refused rather than asked about, for the reason a path that will
            // not resolve is: an approval for a tool nothing answers to could
            // not mean anything. Reaching here means the name came out of a
            // transcript written while the connector was up, or the model
            // invented it — the schemas offered this round only ever name
            // tools that are connected.
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

            // The arguments as the model wrote them, indented so a person can
            // read them. Not "resolved": there is nothing here to resolve, and
            // a dialog that pretended otherwise would be claiming a guarantee
            // this phase does not have.
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
    }
}

/// One line naming a memory, for the approval dialog's header.
///
/// The kind and enough of the sentence to recognize it. The dialog shows the
/// whole thing underneath; this is what the row says when several are queued.
fn summarize_memory(kind: &str, text: &str) -> String {
    const HEAD: usize = 72;

    if text.chars().count() <= HEAD {
        return format!("{kind}: {text}");
    }
    let head: String = text.chars().take(HEAD).collect();
    format!("{kind}: {head}…")
}

/// Resolves a return's artefact paths, and refuses one that leaves the folder.
///
/// Shared by `skill_return` and `handoff_return`, because it is the same claim
/// in both: *these files exist and this run produced them*. Whether they are
/// actually there is the tool's question ([`handoff::check`]); whether they are
/// this workspace's to claim is this one.
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

/// One brief as the approval dialog draws it.
///
/// The goal, the owner and how much it starts from. Not the constraints and not
/// the definition of done: those are what the *owner* has to read, and a dialog
/// that reproduced four whole briefs would be a dialog nobody reads. They are in
/// the file `filed_in` names, and in the transcript of the run.
fn row(brief: &handoff::Brief) -> HandoffRow {
    HandoffRow {
        goal: brief.goal.trim().to_owned(),
        owner: brief.owner.trim().to_owned(),
        priority: brief.priority.as_str().to_owned(),
        return_format: brief.return_format.as_str().to_owned(),
        inputs: u32::try_from(brief.inputs.len()).unwrap_or(u32::MAX),
    }
}

/// One line naming a delegation, for the dialog's header.
///
/// Who, not what: the owners are the decision a person is being asked to make,
/// and the goals are underneath.
fn summarize_handoff(rows: &[HandoffRow], reviewer: Option<&str>) -> String {
    // Each owner once, in the order they were first named. Two briefs to the
    // same identity is an ordinary thing to do — they are two runs, and the
    // list underneath still has a row each — but a header reading "2 briefs to
    // Scribe, Scribe" is a header that looks like a bug.
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

/// The verbs a `git` session grant covers (PLAN 3.3).
///
/// An allow-list, where there used to be a list of verbs that move the tree.
/// That shape covered whatever it had not heard of — `config`, an alias, a verb
/// added in the next release — while the grant's own sentence promised
/// *read-only*. `branch` is not here: it lists or it deletes, and
/// [`GIT_BRANCH_LISTING`] is how the two are told apart.
const GIT_READ_ONLY: &[&str] = &[
    "blame",
    "cat-file",
    "describe",
    "diff",
    "log",
    "ls-files",
    "ls-tree",
    "rev-list",
    "rev-parse",
    "shortlog",
    "show",
    "status",
    "version",
];

/// The options `git branch` may carry and still only list.
const GIT_BRANCH_LISTING: &[&str] = &[
    "--show-current",
    "--list",
    "-l",
    "-a",
    "--all",
    "-r",
    "--remotes",
    "-v",
    "-vv",
    "--verbose",
    "--no-color",
];

/// The options that may come before the verb under a grant.
///
/// Every other one changes where git looks (`-C`, `--git-dir`, `--work-tree`)
/// or which configuration it runs with (`-c`, `--config-env`, `--exec-path`) —
/// and configuration is how git runs programs: `core.fsmonitor` on `status`,
/// `diff.external`, an `!` alias.
const GIT_SAFE_GLOBALS: &[&str] = &[
    "--no-pager",
    "-P",
    "--no-optional-locks",
    "--literal-pathspecs",
    "--version",
];

/// Options of a read-only verb that are not read-only.
///
/// `--output` writes a file wherever it names, `--no-index` and `--contents`
/// read one from anywhere on disk, and `--ext-diff` runs a program.
const GIT_UNSAFE_OPTIONS: &[&str] = &["--output", "--no-index", "--contents", "--ext-diff"];

/// Why a `git` line is not one a session grant may cover, or `None` when it is.
fn git_not_grantable(args: &[String], cwd: &Path, workspace: &Path) -> Option<&'static str> {
    git_line_not_grantable(args).or_else(|| {
        bare_repository_on_the_way(cwd, workspace).then_some(
            "the working directory is laid out like a bare repository, and git would run with \
             whatever configuration is in it",
        )
    })
}

/// The half of [`git_not_grantable`] that only reads the arguments.
fn git_line_not_grantable(args: &[String]) -> Option<&'static str> {
    let mut words = args.iter().map(String::as_str);
    let verb = loop {
        match words.next() {
            // `git` alone prints its usage and opens no repository.
            None => return None,
            Some(word) if GIT_SAFE_GLOBALS.contains(&word) => {}
            Some(word) if word.starts_with('-') => {
                return Some(
                    "an option before the verb changes where git looks, or which configuration \
                     it runs with",
                );
            }
            Some(verb) => break verb,
        }
    };
    let rest: Vec<&str> = words.collect();

    if verb == "branch" {
        if !rest.iter().all(|word| GIT_BRANCH_LISTING.contains(word)) {
            return Some("this `git branch` does more than list branches");
        }
    } else if !GIT_READ_ONLY.contains(&verb) {
        return Some("only read-only verbs (status, log, diff, show, …) are covered");
    }

    let unsafe_option = rest.iter().any(|word| {
        GIT_UNSAFE_OPTIONS.iter().any(|option| {
            word.strip_prefix(option)
                .is_some_and(|tail| tail.is_empty() || tail.starts_with('='))
        })
    });
    unsafe_option.then_some(
        "one of its options writes a file, reads one from anywhere on disk, or runs a program",
    )
}

/// Whether git, started in `cwd`, could find a repository the workspace laid
/// out as ordinary files.
///
/// Git walks up from where it starts and takes the first folder that has a
/// `.git` or *is* one — and a bare repository needs no `.git` at all, only a
/// `HEAD` beside `objects/`. Those are files `fs_write` may create under its
/// grant, so the `config` beside them is one the model could have written, and
/// a `core.fsmonitor` in it runs on `git status`. A `.git` on the way ends the
/// walk: a write there is asked about every time, so what it holds is the
/// user's. So does the workspace root, above which the model writes nothing
/// without a prompt.
fn bare_repository_on_the_way(cwd: &Path, workspace: &Path) -> bool {
    for folder in cwd.ancestors() {
        if !path::is_contained(workspace, folder) {
            return false;
        }
        if fs::symlink_metadata(folder.join(".git")).is_ok() {
            return false;
        }
        if folder.join("HEAD").is_file()
            && (folder.join("objects").is_dir() || folder.join("commondir").is_file())
        {
            return true;
        }
    }
    false
}

/// Whether the program names this application's own binary.
///
/// Compared through [`grants::program_name`], so basename, executable suffix
/// and case on Windows fold the same way everywhere, and `aegis`, `Aegis.exe`
/// and a full path to it are one answer.
fn is_self(ctx: &PolicyCtx<'_>, program: &str) -> bool {
    ctx.self_exe
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .is_some_and(|name| grants::program_name(program) == grants::program_name(name))
}

/// Whether the target is inside the workspace's constitution.
///
/// The first segment only, unlike `.git/` beside it: `world/` is a convention
/// at the root of a workspace, and a repository with a `src/world/` module in
/// it has not thereby written one. The predicate itself is
/// [`world::in_world`], so the gate and the module that reads the constitution
/// agree on what one is.
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
