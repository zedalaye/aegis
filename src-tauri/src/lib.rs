//! Aegis runtime.
//!
//! The WebView renders UI only; the agent loop, tools, policy, secrets and
//! audit live here (`AGENTS.md`). See `docs/architecture.md` for the module
//! map, and PLAN § 6–7 for what each phase added.

pub mod agent;
pub mod approval;
pub mod attach;
pub mod audit;
pub mod board;
pub(crate) mod commands;
pub mod compact;
mod display;
mod error;
pub mod exec_host;
pub mod explorer;
pub mod git;
pub mod handoff;
pub mod intake;
pub mod mcp;
pub mod notify;
pub mod oauth;
pub mod park;
pub mod policy;
pub mod reveal;
pub mod roster;
pub mod schedule;
pub mod secrets;
pub mod skills;
pub mod spend;
mod state;
pub mod store;
pub mod tools;
mod tray;
pub mod weblink;
pub mod workspace;
pub mod world;

pub use agent::{
    Event, EventSink, FakeProvider, ModelCatalog, ModelEvent, ModelRequest, OpenAiProvider,
    Provider, ProviderProbe, Standing, StopReason, SubscriptionProvider, Turn, TurnPlan,
    TurnRegistry, Unattended, Usage,
};
pub use approval::{
    Answer, ApprovalRegistry, ApprovalRequest, Decision as ApprovalDecision, Resolution, ResolvedBy,
};
pub use audit::{AuditArtifact, AuditDecision, AuditEntry, AuditLog, AuditRecord, Outcome};
pub use board::trace::{Run, RunKind, RunRef, RunStatus, SessionLedger};
pub use board::{Board, Facts as BoardFacts, Item as BoardItem, Source as BoardSource};
pub use compact::Plan as CompactionPlan;
pub use error::{AppError, AppResult, ErrorCode};
pub use exec_host::{ExecHost, ExecHostOption, ExecTarget};
pub use git::{Versioning, WorkTree};
pub use handoff::runner::{Delegating, Host as HandoffHost};
pub use handoff::{Brief, Plan as HandoffPlan, Priority, ReturnFormat};
pub use mcp::{Catalog as ConnectorCatalog, ConnectorView, Connectors, State as ConnectorState};
pub use notify::{Note, Notifier, Quiet};
pub use park::{Parking, Parks};
pub use policy::{Decision, Grant, GrantStore, Identity, PolicyCtx, ResolvedCall, ToolCall};
pub use schedule::runner::Scheduler;
pub use secrets::{ApiKey, KeySource, SecretStore};
pub use skills::{ProposalState, Reported, Returned, Skill, SkillCtx, SkillProposal, SkillScope};
pub use state::AppState;
pub use store::{
    Agent, AgentDraft, AgentStore, Attachment, AuthKind, AuthPreset, Compaction, Connector,
    ConnectorDraft, ConnectorStore, Cost, LastRun, MaskedProvider, MaskedSettings, Memory,
    MemoryDraft, MemoryKind, MemoryStore, Message, ParkCause, ParkedAsk, ParkedStore, Project,
    ProjectDetail, ProviderEntry, ProviderSettings, Role, Routine, RoutineDraft, RoutineStore,
    RunOutcome, Schedule, Scheduled, SessionDetail, SessionState, SessionStore, SessionSummary,
    SettingsStore, Store, ToolCallRecord, ToolCallStatus, TurnCost, TurnHandle, DEFAULT_AGENT_ID,
    DEFAULT_PROVIDER_ID,
};
pub use tools::handoff::HandoffCtx;
pub use tools::{NullProgress, ProgressSink, Stream, ToolCtx, ToolOutcome, ToolResult, ToolSpec};
pub use workspace::{ScaffoldReport, WorkspaceEntry, WorkspaceLayout};
pub use world::{SourceState, WorldFile, WorldSource, WorldStatus};

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
/// A real quit (`AppState::is_quitting`) and a missing tray (PLAN 5.3) are let
/// through, so the process never lingers with no way back.
fn on_window_event<R: tauri::Runtime>(window: &tauri::Window<R>, event: &WindowEvent) {
    if let WindowEvent::DragDrop(tauri::DragDropEvent::Drop { paths, position }) = event {
        on_drop(window, paths, *position);
        return;
    }
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

/// Holds the files the OS dropped on the main window, and tells the window
/// (PLAN 7.15).
///
/// Only an id and names reach the WebView; `workspace_import_brief` copies
/// these exact paths.
///
/// Also forbids the dropped paths in the `asset:` scope, which Tauri's own drop
/// handler widens (forbid wins regardless of order) — except paths that are or
/// contain the capture directory, so thumbnails keep working.
fn on_drop<R: tauri::Runtime>(
    window: &tauri::Window<R>,
    paths: &[std::path::PathBuf],
    position: tauri::PhysicalPosition<f64>,
) {
    if window.label() != MAIN_WINDOW || paths.is_empty() {
        return;
    }
    let Some(state) = window.try_state::<AppState>() else {
        return;
    };

    let scope = window.asset_protocol_scope();
    for path in paths {
        let readable = [state.captures(), state.attachments()];
        if readable
            .iter()
            .any(|dir| path.starts_with(dir) || dir.starts_with(path))
        {
            continue;
        }
        let forbidden = if path.is_dir() {
            scope.forbid_directory(path, true)
        } else {
            scope.forbid_file(path)
        };
        if let Err(err) = forbidden {
            tracing::warn!(%err, path = %path.display(), "a dropped path stays readable by the window");
        }
    }

    let names = paths
        .iter()
        .map(|path| {
            path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            )
        })
        .collect();
    let drop_id = state.drops().record(paths.to_vec());

    use agent::event::EventSink as _;
    commands::session::WindowSink::new(window.app_handle().clone()).emit(
        agent::Event::WorkspaceDropped(intake::WorkspaceDropped {
            drop_id,
            names,
            x: position.x,
            y: position.y,
        }),
    );
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
        // Called from Rust only (PLAN 7.22): `capabilities/main.json` grants the
        // window nothing, so the WebView cannot raise a notification itself.
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![
            commands::window::window_toggle,
            commands::window::window_hide,
            commands::window::window_has_tray,
            commands::window::app_quit,
            commands::window::open_url,
            commands::project::project_pick_workspace,
            commands::project::project_create,
            commands::project::project_list,
            commands::project::project_open,
            commands::project::project_delete,
            commands::project::project_list_exec_hosts,
            commands::project::project_set_exec_host,
            commands::agent::agent_list,
            commands::agent::agent_create,
            commands::agent::agent_update,
            commands::agent::agent_delete,
            commands::session::session_create,
            commands::session::session_set_binding,
            commands::session::session_list,
            commands::session::session_open,
            commands::session::session_rename,
            commands::session::session_delete,
            commands::session::session_send,
            commands::attachment::attachment_pick,
            commands::attachment::attachment_drop,
            commands::session::session_cancel,
            commands::session::session_compact,
            commands::approval::approval_list_pending,
            commands::approval::approval_resolve,
            commands::approval::approval_grants,
            commands::approval::approval_revoke_grant,
            commands::audit::audit_tail,
            commands::audit::audit_log_path,
            commands::board::board_read,
            commands::board::board_trace,
            commands::board::board_checkpoint,
            commands::board::board_restore,
            commands::parked::parked_list,
            commands::parked::parked_answer,
            commands::settings::settings_get,
            commands::settings::settings_set,
            commands::settings::settings_add_provider,
            commands::settings::settings_delete_provider,
            commands::settings::settings_clear_key,
            commands::settings::settings_set_prices,
            commands::settings::settings_suggest_prices,
            commands::spend::spend_today,
            commands::settings::settings_probe_provider,
            commands::settings::settings_list_models,
            commands::settings::settings_set_decision,
            commands::settings::settings_clear_decision_key,
            commands::settings::settings_probe_decision,
            commands::workspace::workspace_layout,
            commands::workspace::workspace_scaffold,
            commands::workspace::workspace_reveal,
            commands::workspace::world_status,
            commands::explorer::workspace_tree,
            commands::explorer::workspace_preview,
            commands::explorer::workspace_image,
            commands::explorer::workspace_import_brief,
            commands::skill::skill_list,
            commands::skill::skill_proposals,
            commands::eval::eval_list,
            commands::eval::eval_proposals,
            commands::roster::roster_proposal,
            commands::roster::roster_apply,
            commands::memory::memory_list,
            commands::memory::memory_save,
            commands::memory::memory_forget,
            commands::routine::routine_list,
            commands::routine::routine_save,
            commands::routine::routine_delete,
            commands::routine::routine_set_paused,
            commands::routine::routine_run_now,
            commands::connector::connector_list,
            commands::connector::connector_save,
            commands::connector::connector_delete,
            commands::connector::connector_set_enabled,
            commands::connector::connector_reconnect,
        ])
        .setup(|app| {
            // Built here because the data directory needs the app; the tray
            // and close handler use `try_state`, and commands arrive later.
            let data_dir = app.path().app_data_dir()?;
            tracing::info!(dir = %data_dir.display(), "application data directory");
            let state = AppState::new(&data_dir);

            // The only `asset:` scope (PLAN 5.4, 7.20): captures and
            // attachments, non-recursive. The scope is empty in
            // `tauri.conf.json`.
            for dir in [state.captures(), state.attachments()] {
                if let Err(err) = app.asset_protocol_scope().allow_directory(dir, false) {
                    tracing::warn!(%err, dir = %dir.display(), "images there will not be viewable in the transcript");
                }
            }

            app.manage(state);

            // A missing tray degrades, never breaks (PLAN 5.3): `tray::init`
            // catches the AppIndicator panic, and WSL gets no tray (WSLg hides
            // the window). Only success sets `has_tray`.
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
            // The scheduler (Phase 16), last so state and tray are ready.
            schedule::runner::spawn(app.handle().clone());

            // Connectors start in the background: a cold `npx` must not delay
            // the window.
            commands::connector::spawn(app.handle().clone());

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
        // Keep running after the last window closes only with a tray; an
        // explicit quit (with an exit code) always passes.
        RunEvent::ExitRequested { api, code, .. } if code.is_none() => {
            if app
                .try_state::<AppState>()
                .is_some_and(|state| state.has_tray())
            {
                api.prevent_exit();
                tracing::debug!("exit request without a code ignored; Aegis stays in the tray");
            }
        }
        // Kill connector children explicitly: `AppState` is never dropped, and
        // on Windows children outlive their parent.
        RunEvent::Exit => {
            if let Some(state) = app.try_state::<AppState>() {
                tauri::async_runtime::block_on(state.connectors().shutdown());
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
