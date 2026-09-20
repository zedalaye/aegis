use super::*;

use serde_json::json;

#[test]
fn unknown_tools_are_refused_in_terms_the_model_can_read() {
    let grants = GrantStore::new();
    let ctx = PolicyCtx::new("s1", None, &grants);

    match decide(&ctx, "rm_rf", json!({})) {
        Decision::Deny { code, reason } => {
            assert_eq!(code, ErrorCode::ToolFailed);
            assert!(reason.contains("rm_rf"), "{reason}");
        }
        other => panic!("expected a denial, got {other:?}"),
    }
}

#[test]
fn missing_arguments_are_refused_before_anything_is_resolved() {
    let grants = GrantStore::new();
    let ctx = PolicyCtx::new("s1", None, &grants);

    match decide(&ctx, tool::FS_READ, json!({})) {
        Decision::Deny { code, reason } => {
            assert_eq!(code, ErrorCode::ToolFailed);
            assert!(reason.contains("path"), "{reason}");
        }
        other => panic!("expected a denial, got {other:?}"),
    }
}

#[test]
fn a_stray_argument_does_not_stall_the_turn() {
    let call = ToolCall::parse(tool::FS_LIST, json!({ "path": ".", "depth": 3 }));
    assert!(call.is_ok(), "unknown keys are ignored, not refused");
}

/// The Phase 12 exit condition at the gate: an identity cannot use a tool
/// it was not granted, even when it asks for one directly.
#[test]
fn a_tool_outside_the_identitys_allow_list_is_refused_by_name() {
    let grants = GrantStore::new();
    let workspace = std::env::current_dir().expect("a workspace to measure against");
    let allowed = vec![tool::FS_READ.to_owned(), tool::FS_LIST.to_owned()];
    let ctx = PolicyCtx::new("s1", Some(&workspace), &grants).with_identity(Identity {
        name: "Reviewer",
        tools: &allowed,
        skills: &[],
    });

    match decide(
        &ctx,
        tool::FS_WRITE,
        json!({ "path": "a.txt", "content": "x" }),
    ) {
        Decision::Deny { code, reason } => {
            // `E_DENIED`, not a code of its own: the model already knows
            // what a denial means.
            assert_eq!(code, ErrorCode::Denied);
            assert!(reason.contains("Reviewer"), "{reason}");
            assert!(reason.contains(tool::FS_WRITE), "{reason}");
        }
        other => panic!("expected a denial, got {other:?}"),
    }

    // And the tools it does hold are judged exactly as before.
    assert!(matches!(
        decide(&ctx, tool::FS_LIST, json!({ "path": "." })),
        Decision::Auto { .. }
    ));
}

/// The refusal is about the identity, not about the workspace: it holds
/// when there is no folder to act in either, and it is the answer given.
#[test]
fn the_allow_list_is_checked_before_the_workspace_is() {
    let grants = GrantStore::new();
    let ctx = PolicyCtx::new("s1", None, &grants).with_identity(Identity {
        name: "Scribe",
        tools: &[],
        skills: &[],
    });

    match decide(&ctx, tool::FS_READ, json!({ "path": "a.txt" })) {
        Decision::Deny { code, reason } => {
            assert_eq!(code, ErrorCode::Denied);
            assert!(reason.contains("Scribe"), "{reason}");
        }
        other => panic!("expected a denial, got {other:?}"),
    }
}

/// The Phase 13 half of the same gate: a skill the identity was not
/// granted is refused before the runbook is even located, with no approval
/// offered — and holding every tool does not grant a skill.
#[test]
fn a_skill_outside_the_identitys_allow_list_is_refused_by_name() {
    let grants = GrantStore::new();
    let workspace = std::env::current_dir().expect("a workspace to measure against");
    let every: Vec<String> = crate::tools::names()
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    let held = vec!["inbox.triage".to_owned()];
    let ctx = PolicyCtx::new("s1", Some(&workspace), &grants).with_identity(Identity {
        name: "Triager",
        tools: &every,
        skills: &held,
    });

    match decide(&ctx, tool::SKILL_RUN, json!({ "name": "deploy.draft" })) {
        Decision::Deny { code, reason } => {
            assert_eq!(code, ErrorCode::Denied);
            assert!(reason.contains("Triager"), "{reason}");
            assert!(reason.contains("deploy.draft"), "{reason}");
        }
        other => panic!("expected a denial, got {other:?}"),
    }

    // The one it holds is auto-allowed: loading a runbook it was granted
    // is not a question to put to anyone.
    assert!(matches!(
        decide(&ctx, tool::SKILL_RUN, json!({ "name": "inbox.triage" })),
        Decision::Auto { .. }
    ));
}

/// A name that is not shaped like one never reaches the allow-list, so
/// "granted" and "on the filesystem" cannot be made to disagree.
#[test]
fn a_skill_name_that_is_a_path_is_refused_at_the_door() {
    let call = ToolCall::parse(tool::SKILL_RUN, json!({ "name": "../../etc/passwd" }));
    let reason = call.expect_err("refused");
    assert!(reason.contains("inbox.triage"), "{reason}");
}

/// A decision with no identity behind it is the pre-Phase-12 one. Every
/// policy test written before identities existed relies on this.
#[test]
fn a_decision_with_no_identity_gates_on_the_matrix_alone() {
    let grants = GrantStore::new();
    let workspace = std::env::current_dir().expect("a workspace to measure against");
    let ctx = PolicyCtx::new("s1", Some(&workspace), &grants);

    assert!(ctx.identity.is_none());
    assert!(matches!(
        decide(&ctx, tool::FS_LIST, json!({ "path": "." })),
        Decision::Auto { .. }
    ));
}

#[test]
fn without_a_workspace_nothing_runs() {
    let grants = GrantStore::new();
    let ctx = PolicyCtx::new("s1", None, &grants);

    match decide(&ctx, tool::SCREEN_CAPTURE, json!({})) {
        Decision::Deny { code, .. } => assert_eq!(code, ErrorCode::NoWorkspace),
        other => panic!("expected a denial, got {other:?}"),
    }
}

/// A workspace with a constitution in it and one declared source, already
/// perceived. The world rows of PLAN 7.2 are the four assertions below.
fn with_a_world() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let root = dunce::canonicalize(dir.path()).expect("canonical");

    std::fs::create_dir_all(root.join("world")).expect("world/");
    std::fs::create_dir_all(root.join("sources")).expect("sources/");
    // A cabinet beside it, so the tests can show what a world grant does
    // *not* cover.
    std::fs::create_dir_all(root.join(".aegis/artefacts")).expect("artefacts/");
    std::fs::create_dir_all(root.join(".aegis/status")).expect("status/");
    std::fs::write(root.join("world/essence.md"), "# Essence\n").expect("essence");

    let dump = root.join("sources/dump.sql");
    std::fs::write(&dump, "select 1;\n").expect("dump");
    let digest = format!(
        "{:x}",
        <sha2::Sha256 as sha2::Digest>::digest(b"select 1;\n")
    );
    std::fs::write(
        root.join("world/sources.yml"),
        format!("sources:\n  - path: sources/dump.sql\n    sha256: {digest}\n"),
    )
    .expect("sources.yml");

    (dir, root)
}

/// `COS.md` *Work*: specialists read the world and do not write it. Not an
/// ask with a grant on offer — a refusal, in the same class as a reviewer
/// calling `fs_write`.
#[test]
fn a_brief_cannot_amend_the_world() {
    let (_dir, root) = with_a_world();
    let grants = GrantStore::new();
    let ctx = PolicyCtx::new("s1", Some(&root), &grants).delegated();

    let call = json!({ "path": "world/essence.md", "content": "# Something else\n" });
    match decide(&ctx, tool::FS_WRITE, call) {
        Decision::Deny { code, reason } => {
            assert_eq!(code, ErrorCode::Denied);
            assert!(reason.contains("needs_you"), "{reason}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// Outside a brief, amending the world is asked at high risk, with a grant
/// of its own that covers the constitution and nothing else.
#[test]
fn amending_the_world_is_asked_and_signed_for_on_its_own() {
    let (_dir, root) = with_a_world();
    let grants = GrantStore::new();
    let ctx = PolicyCtx::new("s1", Some(&root), &grants);

    let call = json!({ "path": "world/essence.md", "content": "# Something else\n" });
    match decide(&ctx, tool::FS_WRITE, call.clone()) {
        Decision::Ask { request, .. } => {
            assert_eq!(request.risk, Risk::High);
            assert_eq!(request.grant, Some(Grant::WorldAmend));
            assert!(request.reason.contains("constitution"), "{request:?}");
        }
        other => panic!("expected an ask, got {other:?}"),
    }

    // Signed once, the rest of the session's constitution writes go through.
    grants.insert("s1", Grant::WorldAmend);
    assert!(matches!(
        decide(&ctx, tool::FS_WRITE, call),
        Decision::Auto { .. }
    ));

    // And it covers only that. An ordinary write in the same workspace is
    // still asked about, because the two rows offer different grants.
    let elsewhere = json!({ "path": ".aegis/artefacts/note.md", "content": "hello\n" });
    match decide(&ctx, tool::FS_WRITE, elsewhere) {
        Decision::Ask { request, .. } => assert_eq!(request.grant, Some(Grant::FsWrite)),
        other => panic!("expected the ordinary write row, got {other:?}"),
    }
}

/// The other direction, and the one that matters more: allowing writes so a
/// session can file its artefacts is not agreeing to let it rewrite what
/// the project *is*.
#[test]
fn an_ordinary_write_grant_does_not_reach_the_world() {
    let (_dir, root) = with_a_world();
    let grants = GrantStore::new();
    grants.insert("s1", Grant::FsWrite);
    let ctx = PolicyCtx::new("s1", Some(&root), &grants);

    assert!(
        matches!(
            decide(
                &ctx,
                tool::FS_WRITE,
                json!({ "path": ".aegis/status/STATUS.md", "content": "# Status\n" })
            ),
            Decision::Auto { .. }
        ),
        "the grant covers what it was created for"
    );

    match decide(
        &ctx,
        tool::FS_WRITE,
        json!({ "path": "world/essence.md", "content": "# Something else\n" }),
    ) {
        Decision::Ask { request, .. } => {
            assert_eq!(request.grant, Some(Grant::WorldAmend), "{request:?}");
        }
        other => panic!("the world is still asked about, got {other:?}"),
    }

    // Only the first segment of a path is the constitution, so a repository
    // with its own `src/world/` is untouched by any of this.
    std::fs::create_dir_all(root.join("src/world")).expect("a module of that name");
    assert!(matches!(
        decide(
            &ctx,
            tool::FS_WRITE,
            json!({ "path": "src/world/mod.rs", "content": "//! not one\n" })
        ),
        Decision::Auto { .. }
    ));
}

/// `COS.md`: amending the world is a *human* decision. A scheduled run is
/// the one run with no human in it, so it is offered nothing to sign and
/// refused — the second of two places, the first being the door in
/// `schedule::check` when the routine is saved.
#[test]
fn a_routine_is_offered_nothing_to_sign_for_the_world() {
    let (_dir, root) = with_a_world();
    let grants = GrantStore::new();
    let ctx = PolicyCtx::new("s1", Some(&root), &grants).unattended(true);

    match decide(
        &ctx,
        tool::FS_WRITE,
        json!({ "path": "world/essence.md", "content": "# Something else\n" }),
    ) {
        Decision::Deny { code, reason } => {
            assert_eq!(code, ErrorCode::Denied);
            assert!(reason.contains("nobody is watching"), "{reason}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// The row PLAN 7.2 adds to the read side: a declared source that has not
/// moved is denied rather than asked about, because what it says is already
/// in `world/`.
#[test]
fn a_source_already_perceived_is_refused_and_a_moved_one_reads() {
    let (_dir, root) = with_a_world();
    let grants = GrantStore::new();
    let ctx = PolicyCtx::new("s1", Some(&root), &grants);

    match decide(&ctx, tool::FS_READ, json!({ "path": "sources/dump.sql" })) {
        Decision::Deny { code, reason } => {
            assert_eq!(code, ErrorCode::Denied);
            assert!(reason.contains("sources/dump.sql"), "{reason}");
            assert!(reason.contains("world/"), "{reason}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }

    // The delta is the one legitimate re-perception, so once the operator
    // drops a new dump the same read is an ordinary contained one again.
    std::fs::write(root.join("sources/dump.sql"), "select 1;\nselect 2;\n").expect("write");
    assert!(matches!(
        decide(&ctx, tool::FS_READ, json!({ "path": "sources/dump.sql" })),
        Decision::Auto { .. }
    ));
}

/// And none of it applies to a workspace that never opted in. A folder with
/// no `world/` is judged exactly as it was before this slice.
#[test]
fn a_workspace_with_no_world_is_judged_as_it_always_was() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let root = dunce::canonicalize(dir.path()).expect("canonical");
    std::fs::create_dir_all(root.join("world")).expect("an empty folder of that name");
    std::fs::write(root.join("sources.txt"), "a dump nobody declared").expect("write");

    let grants = GrantStore::new();
    let ctx = PolicyCtx::new("s1", Some(&root), &grants);

    assert!(matches!(
        decide(&ctx, tool::FS_READ, json!({ "path": "sources.txt" })),
        Decision::Auto { .. }
    ));
    // An empty `world/` is not a world, but the gate reads the path, so the
    // first file of a world is asked about like every later one.
    match decide(
        &ctx,
        tool::FS_WRITE,
        json!({ "path": "world/essence.md", "content": "# Essence\n" }),
    ) {
        Decision::Ask { request, .. } => {
            assert_eq!(request.grant, Some(Grant::WorldAmend));
            assert_eq!(request.risk, Risk::High);
        }
        other => panic!("expected an ask, got {other:?}"),
    }
}
/// Without a host nothing moves: the row a `shell_exec` produces is the one
/// every phase before PLAN 7.12 produced, and the dialog has one working
/// directory to show because there is one.
#[test]
fn a_project_on_this_computer_draws_the_row_it_always_did() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let root = dunce::canonicalize(dir.path()).expect("canonical");
    let grants = GrantStore::new();
    let ctx = PolicyCtx::new("s1", Some(&root), &grants);

    match decide(&ctx, tool::SHELL_EXEC, json!({ "program": "git" })) {
        Decision::Ask { call, request } => {
            assert!(matches!(call, ResolvedCall::ShellExec { host: None, .. }));
            match request.detail {
                ApprovalDetail::Shell { host, cwd, .. } => {
                    assert_eq!(host, None);
                    assert_eq!(cwd, root.display().to_string());
                }
                other => panic!("expected a shell row, got {other:?}"),
            }
        }
        other => panic!("expected an ask, got {other:?}"),
    }
}

/// With a host, the dialog names the distribution and the Linux directory
/// beside the Windows folder, and the grant is still keyed on the program.
#[cfg(windows)]
#[test]
fn a_wsl_host_puts_the_distro_and_the_linux_directory_in_the_dialog() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let root = dunce::canonicalize(dir.path()).expect("canonical");
    let grants = GrantStore::new();
    let host = crate::exec_host::ExecHost::Wsl {
        distro: "Ubuntu".to_owned(),
    };
    let ctx = PolicyCtx::new("s1", Some(&root), &grants).with_exec_host(Some(&host));

    let expected = crate::exec_host::linux_path("Ubuntu", &root)
        .expect("a temp dir is on a drive, and a drive is automounted");

    match decide(&ctx, tool::SHELL_EXEC, json!({ "program": "git" })) {
        Decision::Ask { call, request } => {
            match &call {
                ResolvedCall::ShellExec { host, program, .. } => {
                    assert_eq!(program, "git", "the model still names the program");
                    let host = host.as_ref().expect("a host reaches the tool");
                    assert_eq!(host.distro, "Ubuntu");
                    assert_eq!(host.cwd, expected);
                }
                other => panic!("expected a shell call, got {other:?}"),
            }
            assert_eq!(request.grant, Some(Grant::shell("git")));
            assert!(request.reason.contains("Ubuntu"), "{}", request.reason);
            match request.detail {
                ApprovalDetail::Shell { host, cwd, .. } => {
                    assert_eq!(
                        host.map(|host| host.cwd),
                        Some(expected),
                        "the dialog shows the directory the command starts in"
                    );
                    assert_eq!(
                        cwd,
                        root.display().to_string(),
                        "and the folder the file tools use, beside it"
                    );
                }
                other => panic!("expected a shell row, got {other:?}"),
            }
        }
        other => panic!("expected an ask, got {other:?}"),
    }
}

/// A folder of another distribution is refused before anyone is asked: no
/// answer could make the command runnable, and running it here instead is
/// not offered.
#[cfg(windows)]
#[tokio::test]
async fn a_folder_of_another_distro_is_refused_rather_than_asked_about() {
    let Some(installed) = crate::exec_host::installed().await.into_iter().next() else {
        return;
    };

    let dir = tempfile::TempDir::new().expect("temp dir");
    let root = dunce::canonicalize(dir.path()).expect("canonical");
    let grants = GrantStore::new();
    let host = crate::exec_host::ExecHost::Wsl {
        distro: "aegis-not-a-distro".to_owned(),
    };
    let ctx = PolicyCtx::new("s1", Some(&root), &grants).with_exec_host(Some(&host));

    match decide(
        &ctx,
        tool::SHELL_EXEC,
        json!({ "program": "git", "cwd": format!(r"\\wsl$\{installed}\etc") }),
    ) {
        Decision::Deny { code, reason } => {
            assert_eq!(code, ErrorCode::ExecHost, "{reason}");
            assert!(reason.contains("aegis-not-a-distro"), "{reason}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// The same rule arriving by a different road. A build with no WSL cannot
/// honour a WSL host, and every folder on it is one the distribution has no
/// path for — so the refusal is the whole behaviour, and it is a refusal
/// rather than the command quietly running here.
#[cfg(not(windows))]
#[test]
fn a_wsl_host_on_a_build_without_wsl_refuses_before_asking() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let root = dunce::canonicalize(dir.path()).expect("canonical");
    let grants = GrantStore::new();
    let host = crate::exec_host::ExecHost::Wsl {
        distro: "Ubuntu".to_owned(),
    };
    let ctx = PolicyCtx::new("s1", Some(&root), &grants).with_exec_host(Some(&host));

    match decide(&ctx, tool::SHELL_EXEC, json!({ "program": "git" })) {
        Decision::Deny { code, reason } => {
            assert_eq!(code, ErrorCode::ExecHost);
            assert!(reason.contains("Ubuntu"), "{reason}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}
