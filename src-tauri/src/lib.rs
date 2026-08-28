//! Aegis runtime.
//!
//! The WebView renders UI only. The agent loop, tool execution, secrets and
//! policy all live here (AGENTS.md). Phase 0 wires the builder, logging and a
//! single window; later phases add managed state, the tray, commands and the
//! agent loop without changing this entry shape.

use tracing_subscriber::{fmt, prelude::*, EnvFilter};

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

/// Builds and runs the Tauri application.
///
/// # Panics
///
/// Panics only if the Tauri runtime itself fails to start, which is not a
/// recoverable condition for a desktop binary. Everything reachable from a
/// command returns `Result` instead (AGENTS.md: no `unwrap` in library paths).
pub fn run() {
    init_tracing();

    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting Aegis");

    tauri::Builder::default()
        .setup(|_app| {
            tracing::debug!("setup complete");
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running the Aegis application");
}
