//! Aegis runtime.
//!
//! The WebView renders UI only. The agent loop, tool execution, secrets and
//! policy all live here (AGENTS.md). Phase 1 wired logging, managed state, the
//! IPC command surface, the tray and the window lifecycle; Phase 2 adds the
//! project store; Phase 3 adds the approval gate every tool call will pass
//! through; Phase 4 the tool registry, the filesystem tools and the audit log
//! behind them; Phase 5 the sessions, the provider seam and the turn loop that
//! drives the two; Phase 6 the approval registry that turn parks on when
//! policy asks; Phase 7 the shell tool, which is the first one whose running
//! the user watches rather than only its result; Phase 8 the settings, the
//! API key and the OpenAI-compatible provider that finally puts a model behind
//! the loop; and Phase 9 the screen capture tool, the first whose result is a
//! file rather than text — which is why the `asset:` protocol is turned on and
//! scoped, below, to the one directory those files go in. Phase 11 opens the
//! post-MVP sequence (PLAN 7.3) with the shared-workspace convention: files in
//! the user's own folder, read into every request and written by the tools that
//! already exist, rather than a new store. Later phases add commands and
//! modules without changing this entry shape. Phase 12 adds the agent
//! registry: identities as data, a session bound to one, and a tool allow-list
//! the model is filtered by and policy refuses against. Phase 13 adds the
//! skill runner: versioned `SKILL.md` runbooks in a library and in the
//! workspace, a catalog in every request, and the body loaded only into the
//! turn that asked for it.

pub mod agent;
pub mod approval;
pub mod audit;
mod commands;
mod display;
mod error;
pub mod policy;
pub mod secrets;
pub mod skills;
mod state;
pub mod store;
pub mod tools;
mod tray;
pub mod workspace;

pub use agent::{
    Event, EventSink, FakeProvider, ModelEvent, ModelRequest, OpenAiProvider, Provider,
    ProviderProbe, StopReason, Turn, TurnPlan, TurnRegistry, Usage,
};
pub use approval::{
    Answer, ApprovalRegistry, ApprovalRequest, Decision as ApprovalDecision, Resolution, ResolvedBy,
};
pub use audit::{AuditArtifact, AuditDecision, AuditEntry, AuditLog, AuditRecord, Outcome};
pub use error::{AppError, AppResult, ErrorCode};
pub use policy::{Decision, Grant, GrantStore, Identity, PolicyCtx, ResolvedCall, ToolCall};
pub use secrets::{ApiKey, KeySource, SecretStore};
pub use skills::{Skill, SkillCtx, SkillScope};
pub use state::AppState;
pub use store::{
    Agent, AgentDraft, AgentStore, MaskedSettings, Message, Project, ProjectDetail,
    ProviderSettings, Role, SessionDetail, SessionState, SessionStore, SessionSummary,
    SettingsStore, Store, ToolCallRecord, ToolCallStatus, TurnHandle, DEFAULT_AGENT_ID,
    DEFAULT_PROVIDER_ID,
};
pub use tools::{NullProgress, ProgressSink, Stream, ToolCtx, ToolOutcome, ToolResult, ToolSpec};
pub use workspace::{ScaffoldReport, WorkspaceEntry, WorkspaceLayout};

use tauri::{Manager, RunEvent, WindowEvent};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use commands::window::MAIN_WINDOW;

/// Installs the tracing subscriber.
///
/// `AEGIS_LOG` overrides the filter (`AEGIS_LOG=aegis_lib=trace`); the default
/// is `info` for the app and `warn` for everything else, so dependency noise
/// stays out of the console.
fn init_tracing() {
    let filter = EnvFilter::try_from_env("AEGIS_LOG")
        .unwrap_or_else(|_| EnvFilter::new("warn,aegis_lib=info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(
            fmt::layer()
                .with_target(true)
                .with_ansi(cfg!(debug_assertions)),
        )
        .init();
}

/// Turns a close request on the main window into a hide, when there is a
/// tray to come back from.
///
/// Aegis is a tray app: closing the window puts it away, it does not end the
/// session. Two cases are let through instead. A real quit sets
/// `AppState::is_quitting` first — without that check the process would trap
/// its own shutdown and linger with no window and no way back. And if the
/// tray never installed (PLAN 5.3: missing AppIndicator, headless, WSL2),
/// hide-on-close would strand the same way, so the window is allowed to
/// close and the process ends with it.
fn on_window_event<R: tauri::Runtime>(window: &tauri::Window<R>, event: &WindowEvent) {
    let WindowEvent::CloseRequested { api, .. } = event else {
        return;
    };
    if window.label() != MAIN_WINDOW {
        return;
    }
    let stay_resident = window
        .try_state::<AppState>()
        .is_some_and(|state| !state.is_quitting() && state.has_tray());
    if !stay_resident {
        return;
    }

    api.prevent_close();
    match window.hide() {
        Ok(()) => tracing::debug!("close request on the main window handled as hide"),
        Err(err) => tracing::warn!(%err, "could not hide the main window on close"),
    }
}

/// Builds and runs the Tauri application.
///
/// # Panics
///
/// Panics only if the Tauri runtime itself fails to start, which is not a
/// recoverable condition for a desktop binary. Everything reachable from a
/// command returns `Result` instead (AGENTS.md: no `unwrap` in library paths).
pub fn run() {
    init_tracing();
    // Before `Builder::build`: that is when GTK and WebKitGTK initialise,
    // and the DMA-BUF / WSL workarounds are env vars they read once.
    display::prepare();

    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting Aegis");

    let app = tauri::Builder::default()
        // Registered for the runtime's use only: the folder picker is opened
        // by `project_pick_workspace`, and `capabilities/main.json` grants the
        // WebView no `dialog:` permission of its own.
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::window::window_toggle,
            commands::window::window_hide,
            commands::window::window_has_tray,
            commands::window::app_quit,
            commands::project::project_pick_workspace,
            commands::project::project_create,
            commands::project::project_list,
            commands::project::project_open,
            commands::project::project_delete,
            commands::agent::agent_list,
            commands::agent::agent_create,
            commands::agent::agent_update,
            commands::agent::agent_delete,
            commands::session::session_create,
            commands::session::session_list,
            commands::session::session_open,
            commands::session::session_rename,
            commands::session::session_delete,
            commands::session::session_send,
            commands::session::session_cancel,
            commands::approval::approval_list_pending,
            commands::approval::approval_resolve,
            commands::approval::approval_grants,
            commands::approval::approval_revoke_grant,
            commands::audit::audit_tail,
            commands::audit::audit_log_path,
            commands::settings::settings_get,
            commands::settings::settings_set,
            commands::settings::settings_clear_key,
            commands::settings::settings_probe_provider,
            commands::workspace::workspace_layout,
            commands::workspace::workspace_scaffold,
            commands::skill::skill_list,
        ])
        .setup(|app| {
            // State is built here rather than on the builder because loading
            // it needs the application-data directory, and that is only
            // resolvable once there is an app to ask. Nothing observes the
            // gap: the tray and the close handler both reach for the state
            // with `try_state` and cope with its absence, and a command can
            // only arrive once the WebView has loaded, which is after setup.
            let data_dir = app.path().app_data_dir()?;
            tracing::info!(dir = %data_dir.display(), "application data directory");
            let state = AppState::new(&data_dir);

            // The one directory the WebView may read a file from, and the
            // whole reason the `asset:` scheme is enabled at all (PLAN 5.4).
            // A captured PNG reaches the transcript through this protocol
            // rather than through `invoke` as base64, which would copy a
            // megabyte several times and block the IPC channel while it went.
            // Non-recursive, and nothing else is ever added: the scope starts
            // empty in `tauri.conf.json`, so this line is the only thing that
            // widens it, and a capture is the only file it widens it to.
            if let Err(err) = app
                .asset_protocol_scope()
                .allow_directory(state.captures(), false)
            {
                tracing::warn!(%err, "captures will not be viewable in the transcript");
            }

            app.manage(state);

            // A missing tray is a degraded app, not a broken one (PLAN 5.3).
            // Linux AppIndicator bindings panic on a missing `.so`; tray::init
            // catches that. WSL is worse: the .so loads, WSLg maps the
            // indicator as the Windows taskbar icon, and the real window
            // never appears — so we do not install a tray there at all.
            // Only a successful install latches `has_tray`, so close-to-hide
            // cannot trap a process that has no icon.
            if display::is_wsl() {
                tracing::info!("WSL: skipping the tray; the window is the only surface");
            } else {
                match tray::init(app.handle()) {
                    Ok(()) => {
                        if let Some(state) = app.try_state::<AppState>() {
                            state.mark_tray();
                        }
                    }
                    Err(err) => {
                        tracing::warn!(%err, "no tray icon; the window remains the only surface");
                    }
                }
            }
            // WSLg can register a taskbar icon and still leave the window
            // unmapped, off-screen or behind; raising and pinning it here
            // is cheap and is the difference between an icon and a usable UI.
            if let Err(err) = commands::window::show_main(app.handle()) {
                tracing::warn!(%err, "could not raise the main window after setup");
            }
            if let Ok(window) = commands::window::main_window(app.handle()) {
                display::describe_main(&window);
            }
            tracing::debug!("setup complete");
            Ok(())
        })
        .on_window_event(on_window_event)
        .build(tauri::generate_context!())
        .expect("error while building the Aegis application");

    app.run(|app, event| match event {
        // WSLg maps the X11 window to a Windows RAIL surface *after* setup.
        // A show/position during setup reports visible=true at (0,0) and then
        // the compositor never presents it. Re-raise once the event loop is
        // running, after a beat so Weston has the surface.
        RunEvent::Ready if display::is_wsl() => {
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                tracing::info!("WSL: delayed raise after compositor map");
                if let Err(err) = commands::window::show_main(&handle) {
                    tracing::warn!(%err, "WSL delayed raise failed");
                } else if let Ok(window) = commands::window::main_window(&handle) {
                    display::describe_main(&window);
                }
            });
        }
        // Closing the last window must not end the process when the tray is
        // there to bring it back. Without an icon, that same swallow would
        // leave a headless process. An explicit quit passes an exit code,
        // and that is always allowed through.
        RunEvent::ExitRequested { api, code, .. } if code.is_none() => {
            if app
                .try_state::<AppState>()
                .is_some_and(|state| state.has_tray())
            {
                api.prevent_exit();
                tracing::debug!("exit request without a code ignored; Aegis stays in the tray");
            }
        }
        // Clicking the dock icon on macOS is the platform's "come back".
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => {
            if let Err(err) = commands::window::show_main(app) {
                tracing::warn!(%err, "could not show the main window on reopen");
            }
        }
        // Everything else is uninteresting to Aegis today. The discard keeps
        // the handle named on platforms where no arm above reads it.
        _ => {
            let _ = app;
        }
    });
}
