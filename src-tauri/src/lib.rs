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
//! turn that asked for it. Phase 14 completes `COS.md`'s *Bar* with the last
//! of its three: per-agent memory — a store of preferences, exceptions and
//! conventions scoped to one identity, in every request and written under the
//! gate — and compaction, which folds a long session's older turns into coded
//! state rather than asking a model to summarize them. Phase 15 is the first
//! that builds *on* that bar rather than towards it: the handoff bus, where one
//! identity hands briefs to others and reads back a board of statuses instead
//! of their conversations. A delegated run is an ordinary session under the
//! owner's own identity, driven by the same turn loop and gated by the same
//! matrix — there is no second agent loop here, and that is the design. Phase
//! 16 puts a clock on that same loop: a routine fires a granted skill on a
//! schedule or when a folder changes, in a session nobody is watching, which is
//! why it is also the phase where policy stops asking and starts refusing —
//! what an unattended run may do is exactly what a person signed onto the
//! routine, and the tray process outliving the window is what makes any of it
//! possible. Phase 17 is the read of all of that — a board of what needs a
//! person, and a run folded out of the audit log. Phase 18 fills in `mcp/`:
//! external MCP servers, started by this process, whose tools reach the model
//! through the same registry and the same approval dialog as `fs_write`. It
//! is the first phase whose tools this repository did not write, which is why
//! every one of them asks. PLAN 7.15 is not a phase, and it is the one change
//! here to the window's own event handling: a file dropped on the window is
//! held by the runtime until the window names it, then copied into
//! `.aegis/briefs/` — beside the explorer's read-only tree and preview. PLAN
//! 7.14 is not a phase either: `roster` turns a workspace's roster proposal
//! into identity rows when a person applies it, and no tool reaches it.

pub mod agent;
pub mod approval;
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
pub mod oauth;
pub mod policy;
pub mod reveal;
pub mod roster;
pub mod schedule;
pub mod secrets;
pub mod skills;
mod state;
pub mod store;
pub mod tools;
mod tray;
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
pub use policy::{Decision, Grant, GrantStore, Identity, PolicyCtx, ResolvedCall, ToolCall};
pub use schedule::runner::Scheduler;
pub use secrets::{ApiKey, KeySource, SecretStore};
pub use skills::{ProposalState, Reported, Returned, Skill, SkillCtx, SkillProposal, SkillScope};
pub use state::AppState;
pub use store::{
    Agent, AgentDraft, AgentStore, AuthKind, AuthPreset, Compaction, Connector, ConnectorDraft,
    ConnectorStore, Cost, LastRun, MaskedSettings, Memory, MemoryDraft, MemoryKind, MemoryStore,
    Message, Project, ProjectDetail, ProviderSettings, Role, Routine, RoutineDraft, RoutineStore,
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
/// Aegis is a tray app: closing the window puts it away, it does not end the
/// session. Two cases are let through instead. A real quit sets
/// `AppState::is_quitting` first — without that check the process would trap
/// its own shutdown and linger with no window and no way back. And if the
/// tray never installed (PLAN 5.3: missing AppIndicator, headless, WSL2),
/// hide-on-close would strand the same way, so the window is allowed to
/// close and the process ends with it.
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
/// The paths are kept here, under an id, and only the id and the names go to
/// the WebView. `workspace_import_brief` then copies exactly these paths and
/// nothing the window could have typed — which is what "the command takes a
/// path the OS already handed the process" means in practice.
///
/// It also undoes something Tauri does on the way. Its own drop handler
/// widens the `asset:` protocol scope to every dropped path, recursively for a
/// folder, so the WebView could load them. Nothing here wants that: the scope
/// is the capture directory and nothing else (`setup`, below), and a brief is
/// copied by the runtime rather than read by the window. A forbidden pattern
/// beats an allowed one in that scope whichever is registered first, so the
/// order the two handlers run in does not matter. A path that is, or holds,
/// the capture directory is left alone, or a drop of the data folder would
/// blank every thumbnail in the transcript.
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
        if path.starts_with(state.captures()) || state.captures().starts_with(path) {
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
            commands::project::project_list_exec_hosts,
            commands::project::project_set_exec_host,
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
            commands::session::session_compact,
            commands::approval::approval_list_pending,
            commands::approval::approval_resolve,
            commands::approval::approval_grants,
            commands::approval::approval_revoke_grant,
            commands::audit::audit_tail,
            commands::audit::audit_log_path,
            commands::board::board_read,
            commands::board::board_trace,
            commands::settings::settings_get,
            commands::settings::settings_set,
            commands::settings::settings_clear_key,
            commands::settings::settings_probe_provider,
            commands::settings::settings_list_models,
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
            // Last, so it starts against a state that is fully built and a tray
            // that has already had its chance to install. It is the one part of
            // the runtime that acts without being asked (PLAN 7.3, Phase 16):
            // one task, woken every half minute, that fires a routine when one
            // is due and does nothing at all the rest of the time. A build with
            // no routines never notices it.
            schedule::runner::spawn(app.handle().clone());

            // And the connectors, which are the other thing that acts without
            // being asked — though only in the sense that a process is started.
            // A connector answers questions; it never opens a session. Started
            // last and in the background because an `npx` fetching a package on
            // a cold cache is a minute nobody should wait for the window on,
            // and a connector that never comes up costs a row in Settings
            // rather than a boot.
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
        // The process is going. Connector children are spawned with
        // `kill_on_drop`, but nothing drops `AppState` on the way out — the
        // process simply ends — and on Windows a child outlives the parent that
        // started it. So they are ended explicitly here, which is the
        // difference between quitting Aegis and leaving four `node` processes
        // behind. Bounded: killing a child is a syscall, not a wait.
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
