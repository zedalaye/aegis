//! The `shell_exec` rows of the table (PLAN 3, PLAN 3.1, PLAN 7.12).

use super::*;

/// `ShellExec`'s rows.
pub(super) fn exec(
    ctx: &PolicyCtx<'_>,
    workspace: &Path,
    program: String,
    args: Vec<String>,
    cwd: Option<String>,
    timeout_ms: Option<u64>,
) -> Result<Decision, Decision> {
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

    // Translate the directory for the host (PLAN 7.12), or refuse:
    // running the command here instead must never be an option.
    let host = match ctx.exec_host {
        Some(ExecHost::Wsl { distro }) => match exec_host::linux_path(distro, &directory.path) {
            Ok(cwd) => Some(ExecTarget {
                distro: distro.clone(),
                cwd,
            }),
            Err(reason) => return Err(Decision::deny(ErrorCode::ExecHost, reason)),
        },
        None => None,
    };

    // A program named by a path is keyed on where it resolves (PLAN
    // 3.1) — against the working directory, or in the distribution's
    // spelling — so a workspace file called `git` is not `git`.
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

    // A git grant covers read-only lines only (PLAN 3.1, IDEAS.md § 12).
    // The verb is not enough: `-c core.fsmonitor=…` makes `git status`
    // run a program, and a planted bare repository brings its config.
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
                reason: format!("allowing git for the session does not cover this line: {why}"),
            },
        ));
    }

    // A host changes the wording, never the key: no grant is on
    // `wsl.exe`.
    let grant = Grant::shell(&key);
    let reason = match ctx.exec_host {
        Some(ExecHost::Wsl { distro }) => format!(
            "a command runs in `{distro}` as that distribution's own user, and is not sandboxed"
        ),
        None => "a command runs with your own privileges and is not sandboxed".to_owned(),
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

/// The verbs a `git` session grant covers (PLAN 3.1). An allow-list: the old
/// deny-list covered verbs it had never heard of. `branch` is judged by
/// [`GIT_BRANCH_LISTING`], since it lists or deletes.
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

/// The options allowed before the verb under a grant. Any other changes where
/// git looks or which configuration it runs with — and configuration runs
/// programs (`core.fsmonitor`, `diff.external`, `!` aliases).
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
pub(super) fn git_not_grantable(
    args: &[String],
    cwd: &Path,
    workspace: &Path,
) -> Option<&'static str> {
    git_line_not_grantable(args).or_else(|| {
        bare_repository_on_the_way(cwd, workspace).then_some(
            "the working directory is laid out like a bare repository, and git would run with \
             whatever configuration is in it",
        )
    })
}

/// The half of [`git_not_grantable`] that only reads the arguments.
pub(super) fn git_line_not_grantable(args: &[String]) -> Option<&'static str> {
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

/// Whether git started in `cwd` could find a bare repository laid out as
/// ordinary workspace files (a `HEAD` beside `objects/`), whose `config` the
/// model could have written. A `.git` on the way ends git's search, and the
/// walk stops at the workspace root.
pub(super) fn bare_repository_on_the_way(cwd: &Path, workspace: &Path) -> bool {
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
pub(super) fn is_self(ctx: &PolicyCtx<'_>, program: &str) -> bool {
    ctx.self_exe
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .is_some_and(|name| grants::program_name(program) == grants::program_name(name))
}
