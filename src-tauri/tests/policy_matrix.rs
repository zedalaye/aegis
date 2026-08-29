//! The decision table of PLAN 3, row by row.
//!
//! Each row of the matrix gets a test named after the sentence it encodes, and
//! the hard denials of PLAN 3.2 get one each as well. Two properties are worth
//! more than any individual row and are tested separately at the bottom:
//!
//! * a session grant can only ever collapse an ask the table already offered
//!   it for — it can never reach outside the workspace or into `.git/`;
//! * grants belong to one session and never leak into another.

use std::fs;
use std::path::{Path, PathBuf};

use aegis_lib::policy::{
    decide, decide_call, tool, ApprovalDetail, AskRequest, Decision, Grant, GrantStore, PolicyCtx,
    ResolvedCall, Risk, ScreenGeometry, ToolCall,
};
use aegis_lib::ErrorCode;
use serde_json::json;
use tempfile::TempDir;

/// A workspace, a directory outside it, and a grant store.
struct Fixture {
    _workspace_guard: TempDir,
    _outside_guard: TempDir,
    workspace: PathBuf,
    outside: PathBuf,
    grants: GrantStore,
}

impl Fixture {
    fn new() -> Self {
        let workspace_guard = TempDir::new().expect("temp dir");
        let outside_guard = TempDir::new().expect("temp dir");
        Self {
            workspace: dunce::canonicalize(workspace_guard.path()).expect("canonical"),
            outside: dunce::canonicalize(outside_guard.path()).expect("canonical"),
            _workspace_guard: workspace_guard,
            _outside_guard: outside_guard,
            grants: GrantStore::new(),
        }
    }

    fn ctx(&self) -> PolicyCtx<'_> {
        PolicyCtx::new("session-1", Some(&self.workspace), &self.grants)
    }

    /// Creates a file in the workspace, parents included.
    fn file(&self, relative: &str, content: &str) -> PathBuf {
        let path = self.workspace.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(&path, content).expect("write");
        path
    }

    /// A path outside the workspace, as the model would spell it.
    fn outside_path(&self, relative: &str) -> String {
        self.outside.join(relative).to_string_lossy().into_owned()
    }
}

/// Unwraps an ask, or explains what came back instead.
fn ask(decision: Decision) -> AskRequest {
    match decision {
        Decision::Ask { request, .. } => *request,
        other => panic!("expected an approval request, got {other:?}"),
    }
}

/// Unwraps an auto-allow's resolved call.
fn auto(decision: Decision) -> ResolvedCall {
    match decision {
        Decision::Auto { call, .. } => call,
        other => panic!("expected an auto-allow, got {other:?}"),
    }
}

/// Unwraps a refusal's code.
fn denied(decision: Decision) -> ErrorCode {
    match decision {
        Decision::Deny { code, .. } => code,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

// ---------------------------------------------------------------- fs_list --

#[test]
fn listing_inside_the_workspace_is_automatic() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.workspace.join("src")).expect("mkdir");

    let decision = decide(&fixture.ctx(), tool::FS_LIST, json!({ "path": "src" }));

    assert_eq!(
        auto(decision),
        ResolvedCall::FsList {
            path: fixture.workspace.join("src"),
            max_entries: None,
        }
    );
}

#[test]
fn listing_outside_the_workspace_asks_every_time() {
    let fixture = Fixture::new();

    let decision = decide(
        &fixture.ctx(),
        tool::FS_LIST,
        json!({ "path": fixture.outside_path("") }),
    );
    let request = ask(decision);

    assert_eq!(request.tool, tool::FS_LIST);
    assert_eq!(request.risk, Risk::Medium);
    assert_eq!(request.grant, None, "no session grant reaches outside");
    assert!(matches!(request.detail, ApprovalDetail::FsList { .. }));
}

// ---------------------------------------------------------------- fs_read --

#[test]
fn reading_an_ordinary_file_inside_the_workspace_is_automatic() {
    let fixture = Fixture::new();
    fixture.file("src/main.rs", "fn main() {}");

    let decision = decide(
        &fixture.ctx(),
        tool::FS_READ,
        json!({ "path": "src/main.rs" }),
    );

    assert!(matches!(auto(decision), ResolvedCall::FsRead { .. }));
}

#[test]
fn reading_a_credential_shaped_name_asks_even_inside_the_workspace() {
    let fixture = Fixture::new();
    fixture.file(".env", "TOKEN=abc");

    let request = ask(decide(
        &fixture.ctx(),
        tool::FS_READ,
        json!({ "path": ".env" }),
    ));

    assert_eq!(request.risk, Risk::High);
    assert_eq!(
        request.grant, None,
        "a secret is asked about every single time"
    );
}

#[test]
fn a_credential_shaped_directory_on_the_way_counts_too() {
    let fixture = Fixture::new();
    fixture.file(".ssh/config", "Host *");

    let request = ask(decide(
        &fixture.ctx(),
        tool::FS_READ,
        json!({ "path": ".ssh/config" }),
    ));

    assert_eq!(request.risk, Risk::High);
    assert_eq!(request.grant, None);
}

#[test]
fn the_sensitive_predicate_reads_only_below_the_workspace_root() {
    // The user chose the workspace; one of its own ancestors being called
    // `credentials` must not tag every file inside it, or the prompt stops
    // meaning anything.
    let guard = TempDir::new().expect("temp dir");
    let workspace = dunce::canonicalize(guard.path())
        .expect("canonical")
        .join("credentials");
    fs::create_dir(&workspace).expect("mkdir");
    fs::write(workspace.join("notes.md"), "hello").expect("write");

    let grants = GrantStore::new();
    let ctx = PolicyCtx::new("session-1", Some(&workspace), &grants);

    let decision = decide(&ctx, tool::FS_READ, json!({ "path": "notes.md" }));

    assert!(matches!(auto(decision), ResolvedCall::FsRead { .. }));
}

#[test]
fn reading_a_large_file_asks_but_offers_a_grant() {
    let fixture = Fixture::new();
    fixture.file("big.bin", &"x".repeat(1024 * 1024 + 1));

    let request = ask(decide(
        &fixture.ctx(),
        tool::FS_READ,
        json!({ "path": "big.bin" }),
    ));

    assert_eq!(request.risk, Risk::Low);
    assert_eq!(request.grant, Some(Grant::FsReadLarge));
    assert!(
        request.scope_label.contains("1 MB"),
        "{}",
        request.scope_label
    );
}

#[test]
fn a_large_secret_is_treated_as_a_secret_first() {
    let fixture = Fixture::new();
    fixture.file("keys/server.pem", &"x".repeat(1024 * 1024 + 1));

    let request = ask(decide(
        &fixture.ctx(),
        tool::FS_READ,
        json!({ "path": "keys/server.pem" }),
    ));

    assert_eq!(request.risk, Risk::High);
    assert_eq!(
        request.grant, None,
        "the row without a grant has to win over the row with one"
    );
}

#[test]
fn reading_outside_the_workspace_asks_every_time() {
    let fixture = Fixture::new();
    fs::write(fixture.outside.join("notes.md"), "hello").expect("write");

    let request = ask(decide(
        &fixture.ctx(),
        tool::FS_READ,
        json!({ "path": fixture.outside_path("notes.md") }),
    ));

    assert_eq!(request.risk, Risk::High);
    assert_eq!(request.grant, None);
}

// --------------------------------------------------------------- fs_write --

#[test]
fn writing_a_new_file_inside_the_workspace_asks_and_offers_a_grant() {
    let fixture = Fixture::new();

    let request = ask(decide(
        &fixture.ctx(),
        tool::FS_WRITE,
        json!({ "path": "notes.md", "content": "hello" }),
    ));

    assert_eq!(request.risk, Risk::Medium);
    assert_eq!(request.grant, Some(Grant::FsWrite));
    match request.detail {
        ApprovalDetail::FsWrite {
            bytes,
            exists,
            preview,
            ..
        } => {
            assert_eq!(bytes, 5);
            assert!(!exists);
            assert_eq!(preview.as_deref(), Some("hello"));
        }
        other => panic!("expected a write detail, got {other:?}"),
    }
}

#[test]
fn overwriting_says_so_in_the_prompt() {
    let fixture = Fixture::new();
    fixture.file("notes.md", "old");

    let request = ask(decide(
        &fixture.ctx(),
        tool::FS_WRITE,
        json!({ "path": "notes.md", "content": "new" }),
    ));

    assert!(request.summary.contains("overwrite"), "{}", request.summary);
    assert!(matches!(
        request.detail,
        ApprovalDetail::FsWrite { exists: true, .. }
    ));
}

#[test]
fn writing_inside_dot_git_asks_every_time_with_no_grant() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.workspace.join(".git")).expect("mkdir");

    let request = ask(decide(
        &fixture.ctx(),
        tool::FS_WRITE,
        json!({ "path": ".git/config", "content": "[core]" }),
    ));

    assert_eq!(request.risk, Risk::High);
    assert_eq!(request.grant, None);
    assert!(request.reason.contains(".git"), "{}", request.reason);
}

#[test]
fn writing_outside_the_workspace_asks_every_time_with_no_grant() {
    let fixture = Fixture::new();

    let request = ask(decide(
        &fixture.ctx(),
        tool::FS_WRITE,
        json!({ "path": fixture.outside_path("notes.md"), "content": "hello" }),
    ));

    assert_eq!(request.risk, Risk::High);
    assert_eq!(request.grant, None);
}

#[test]
fn a_credential_shaped_write_raises_the_badge_without_changing_the_gate() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.workspace.join(".ssh")).expect("mkdir");

    let request = ask(decide(
        &fixture.ctx(),
        tool::FS_WRITE,
        json!({ "path": ".ssh/id_rsa", "content": "-----BEGIN" }),
    ));

    assert_eq!(request.risk, Risk::High);
    assert_eq!(
        request.grant,
        Some(Grant::FsWrite),
        "the badge is advisory; the row is still the contained-write row"
    );
}

// ------------------------------------------------------------ shell_exec --

#[test]
fn a_command_in_the_workspace_asks_and_offers_a_grant_on_the_program() {
    let fixture = Fixture::new();

    let request = ask(decide(
        &fixture.ctx(),
        tool::SHELL_EXEC,
        json!({ "program": "git", "args": ["status"] }),
    ));

    assert_eq!(request.risk, Risk::High);
    assert_eq!(request.grant, Some(Grant::shell("git")));
    assert_eq!(request.summary, "git status");
    match request.detail {
        ApprovalDetail::Shell { cwd, .. } => {
            assert_eq!(Path::new(&cwd), fixture.workspace);
        }
        other => panic!("expected a shell detail, got {other:?}"),
    }
}

#[test]
fn a_command_outside_the_workspace_asks_every_time_with_no_grant() {
    let fixture = Fixture::new();

    let request = ask(decide(
        &fixture.ctx(),
        tool::SHELL_EXEC,
        json!({ "program": "git", "args": ["status"], "cwd": fixture.outside_path("") }),
    ));

    assert_eq!(request.risk, Risk::High);
    assert_eq!(request.grant, None);
}

// --------------------------------------------------------- screen_capture --

#[test]
fn capturing_the_screen_asks_and_names_the_display() {
    let fixture = Fixture::new();
    let screen = ScreenGeometry {
        display: "the built-in display".to_owned(),
        width: 2560,
        height: 1440,
        logical_width: 1707,
        logical_height: 960,
    };
    let ctx = fixture.ctx().with_screen(Some(&screen));

    let request = ask(decide(&ctx, tool::SCREEN_CAPTURE, json!({})));

    assert_eq!(request.risk, Risk::Medium);
    assert_eq!(request.grant, Some(Grant::ScreenCapture));
    assert_eq!(
        request.detail,
        ApprovalDetail::Screen {
            display: "the built-in display".to_owned(),
            width: 2560,
            height: 1440,
            logical_width: 1707,
            logical_height: 960,
        }
    );
    assert!(request.summary.contains("2560"), "{}", request.summary);
}

#[test]
fn an_unknown_display_is_refused_rather_than_guessed_at() {
    let fixture = Fixture::new();

    let decision = decide(
        &fixture.ctx(),
        tool::SCREEN_CAPTURE,
        json!({ "display": "hdmi-2" }),
    );

    assert_eq!(denied(decision), ErrorCode::ToolFailed);
}

// ------------------------------------------------------------ 3.2 denials --

#[test]
fn nothing_runs_without_a_workspace() {
    let grants = GrantStore::new();
    let ctx = PolicyCtx::new("session-1", None, &grants);

    let decision = decide(&ctx, tool::FS_LIST, json!({ "path": "." }));

    assert_eq!(denied(decision), ErrorCode::NoWorkspace);
}

#[test]
fn a_link_that_escapes_the_workspace_is_refused_without_a_prompt() {
    let fixture = Fixture::new();
    fs::write(fixture.outside.join("secret.txt"), "s3cret").expect("write");

    let link = fixture.workspace.join("escape");
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_dir(&fixture.outside, &link);
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&fixture.outside, &link);
    if made.is_err() {
        eprintln!("skipping: this machine does not allow creating symlinks");
        return;
    }

    let decision = decide(
        &fixture.ctx(),
        tool::FS_READ,
        json!({ "path": "escape/secret.txt" }),
    );

    assert_eq!(
        denied(decision),
        ErrorCode::PathOutsideWorkspace,
        "there is no honest way to show this one in an approval dialog"
    );
}

#[test]
fn writing_onto_a_directory_is_refused() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.workspace.join("src")).expect("mkdir");

    let decision = decide(
        &fixture.ctx(),
        tool::FS_WRITE,
        json!({ "path": "src", "content": "hello" }),
    );

    assert_eq!(denied(decision), ErrorCode::PathInvalid);
}

#[test]
fn writing_into_a_missing_folder_is_refused_unless_it_may_be_created() {
    let fixture = Fixture::new();

    let refused = decide(
        &fixture.ctx(),
        tool::FS_WRITE,
        json!({ "path": "generated/report.md", "content": "hello" }),
    );
    assert_eq!(denied(refused), ErrorCode::PathInvalid);

    let asked = decide(
        &fixture.ctx(),
        tool::FS_WRITE,
        json!({ "path": "generated/report.md", "content": "hello", "create_dirs": true }),
    );
    assert_eq!(ask(asked).grant, Some(Grant::FsWrite));
}

#[test]
fn an_empty_program_is_refused() {
    let fixture = Fixture::new();

    let decision = decide(&fixture.ctx(), tool::SHELL_EXEC, json!({ "program": "  " }));

    assert_eq!(denied(decision), ErrorCode::Denied);
}

#[test]
fn aegis_will_not_run_itself() {
    let fixture = Fixture::new();
    let exe = if cfg!(windows) {
        PathBuf::from(r"C:\Program Files\Aegis\aegis.exe")
    } else {
        PathBuf::from("/opt/aegis/aegis")
    };
    let ctx = fixture.ctx().with_self_exe(Some(&exe));

    assert_eq!(
        denied(decide(
            &ctx,
            tool::SHELL_EXEC,
            json!({ "program": "aegis" })
        )),
        ErrorCode::Denied
    );
    assert_eq!(
        denied(decide(
            &ctx,
            tool::SHELL_EXEC,
            json!({ "program": exe.to_string_lossy() })
        )),
        ErrorCode::Denied
    );
}

#[test]
fn a_command_needs_a_working_directory_that_exists() {
    let fixture = Fixture::new();

    let decision = decide(
        &fixture.ctx(),
        tool::SHELL_EXEC,
        json!({ "program": "git", "cwd": "nowhere" }),
    );

    assert_eq!(denied(decision), ErrorCode::PathInvalid);
}

// ------------------------------------------------------------- 3.1 grants --

#[test]
fn a_grant_collapses_the_ask_it_was_offered_for() {
    let fixture = Fixture::new();
    let call = || {
        decide(
            &fixture.ctx(),
            tool::FS_WRITE,
            json!({ "path": "notes.md", "content": "hello" }),
        )
    };

    let request = ask(call());
    let grant = request.grant.expect("this row offers a grant");
    fixture.grants.insert("session-1", grant);

    assert!(matches!(auto(call()), ResolvedCall::FsWrite { .. }));
}

#[test]
fn a_write_grant_never_reaches_outside_the_workspace_or_into_dot_git() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.workspace.join(".git")).expect("mkdir");
    fixture.grants.insert("session-1", Grant::FsWrite);

    let outside = ask(decide(
        &fixture.ctx(),
        tool::FS_WRITE,
        json!({ "path": fixture.outside_path("notes.md"), "content": "hello" }),
    ));
    assert_eq!(outside.grant, None);

    let git = ask(decide(
        &fixture.ctx(),
        tool::FS_WRITE,
        json!({ "path": ".git/config", "content": "[core]" }),
    ));
    assert_eq!(git.grant, None);
}

#[test]
fn a_shell_grant_covers_one_program_and_no_other() {
    let fixture = Fixture::new();
    fixture.grants.insert("session-1", Grant::shell("git"));

    let granted = decide(
        &fixture.ctx(),
        tool::SHELL_EXEC,
        json!({ "program": "git", "args": ["log"] }),
    );
    assert!(matches!(auto(granted), ResolvedCall::ShellExec { .. }));

    let other = decide(
        &fixture.ctx(),
        tool::SHELL_EXEC,
        json!({ "program": "rm", "args": ["-rf", "."] }),
    );
    assert_eq!(ask(other).grant, Some(Grant::shell("rm")));
}

#[test]
fn a_grant_belongs_to_the_session_that_made_it() {
    let fixture = Fixture::new();
    fixture.grants.insert("session-2", Grant::FsWrite);

    let decision = decide_call(
        &fixture.ctx(),
        ToolCall::FsWrite {
            path: "notes.md".to_owned(),
            content: "hello".to_owned(),
            create_dirs: false,
        },
    );

    assert_eq!(
        ask(decision).grant,
        Some(Grant::FsWrite),
        "another session's approval must not answer this one"
    );
}

#[test]
fn revoking_a_grant_brings_the_prompt_back() {
    let fixture = Fixture::new();
    fixture.grants.insert("session-1", Grant::FsWrite);
    let call = || {
        decide(
            &fixture.ctx(),
            tool::FS_WRITE,
            json!({ "path": "notes.md", "content": "hello" }),
        )
    };
    assert!(matches!(auto(call()), ResolvedCall::FsWrite { .. }));

    assert!(fixture.grants.revoke("session-1", &Grant::FsWrite));

    assert_eq!(ask(call()).grant, Some(Grant::FsWrite));
}
