//! The `fs_*` rows of the table (PLAN 3).

use super::*;

/// `FsList`'s rows.
pub(super) fn list(
    workspace: &Path,
    path: String,
    max_entries: Option<u32>,
) -> Result<Decision, Decision> {
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

/// `FsRead`'s rows.
pub(super) fn read(
    workspace: &Path,
    path: String,
    offset: Option<u64>,
    limit: Option<u64>,
) -> Result<Decision, Decision> {
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

    // A declared source of a world, unchanged since it was perceived
    // (PLAN 7.2): refused, because what it says is already in `world/`.
    // A source that moved falls through, so its delta can be read.
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

/// `FsWrite`'s rows.
pub(super) fn write(
    ctx: &PolicyCtx<'_>,
    workspace: &Path,
    path: String,
    content: String,
    create_dirs: bool,
) -> Result<Decision, Decision> {
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

    // The constitution (PLAN 7.2, `COS.md` *Work*). Which row applies
    // depends on the run, not the identity: inside a brief it is a
    // refusal, and the specialist returns `needs_you`.
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

        // In a session it is a human decision, so a dialog — with a
        // grant of its own, never `FsWrite`'s, so founding a world is
        // not six identical prompts. Unattended there is no grant, so
        // `decide_call` refuses; `schedule::check` also refuses to store
        // one on a routine.
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

    // Applying a skill proposal (PLAN 7.13): asked every time with no
    // grant, so a held `FsWrite` never makes a runbook live unseen. A
    // brief hands the proposal back instead; a routine is refused.
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

    // A runnable eval (PLAN 7.18): applying a proposal is asked every
    // time, like a skill's; any other `eval.yml` write is too, since
    // it would make questions live that nobody signed as a proposal.
    let eval_write = target
        .relative_to(workspace)
        .and_then(|relative| eval::write_of(workspace, &relative, call_content(&call)));
    if let Some(eval_write) = eval_write {
        let (name, title, reason) = match eval_write {
            EvalWrite::Refused(reason) => return Err(Decision::deny(ErrorCode::Denied, reason)),
            EvalWrite::Apply(name) => {
                let reason = format!(
                    "this copies a proposal to `eval.yml`, which makes `{name}` an eval \
                     `jev_eval` can run. Read the questions and thresholds: they are \
                     what you sign. It grants it to no identity"
                );
                (name, "Apply an eval proposal", reason)
            }
            EvalWrite::Direct(name) => {
                let reason = format!(
                    "this writes `{name}`'s `eval.yml` directly, not from a proposal: \
                     whatever questions it holds become runnable"
                );
                (name, "Write a project eval", reason)
            }
        };
        if ctx.delegated {
            return Err(Decision::deny(
                ErrorCode::Denied,
                format!(
                    "you are working on a brief, and a brief does not make an eval \
                     runnable. Write `.aegis/evals/{name}/PROPOSAL.yml`, return it in \
                     `artefacts`, and leave applying it to a person"
                ),
            ));
        }
        return Ok(ask(
            call,
            AskRequest {
                tool: tool::FS_WRITE.to_owned(),
                risk: Risk::High,
                title,
                summary,
                detail,
                grant: None,
                scope_label: scope_label(None),
                reason,
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
            // The badge only: a contained write is asked about anyway.
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
