//! Window and application lifecycle commands.
//!
//! Aegis lives in the tray, so the main window's visibility is runtime state,
//! not a WebView concern: `capabilities/main.json` deliberately grants no
//! `core:window` permissions, and the WebView reaches the window only through
//! the commands below. That keeps one code path — used by the tray, the
//! close handler and the UI alike — for showing and hiding.

use tauri::{AppHandle, Manager, Runtime, WebviewWindow};

use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// Label of the one window the MVP creates (`tauri.conf.json`).
pub const MAIN_WINDOW: &str = "main";

/// Looks up the main window, or fails with a structured error rather than an
/// `unwrap` on an `Option` that is genuinely empty during shutdown.
pub fn main_window<R: Runtime>(app: &AppHandle<R>) -> AppResult<WebviewWindow<R>> {
    app.get_webview_window(MAIN_WINDOW)
        .ok_or_else(|| AppError::WindowUnavailable {
            label: MAIN_WINDOW.to_owned(),
        })
}

/// Shows the main window and gives it focus, un-minimizing first.
///
/// `show` alone leaves a minimized window minimized, and a window raised
/// without focus is indistinguishable from nothing happening — both are
/// reported as "the tray icon is broken".
pub fn show_main<R: Runtime>(app: &AppHandle<R>) -> AppResult<()> {
    let window = main_window(app)?;

    if window.is_minimized().unwrap_or(false) {
        window.unminimize()?;
    }
    window.show()?;
    window.set_focus()?;

    tracing::debug!(window = MAIN_WINDOW, "shown");
    Ok(())
}

/// Hides the main window. The process keeps running; the tray brings it back.
pub fn hide_main<R: Runtime>(app: &AppHandle<R>) -> AppResult<()> {
    main_window(app)?.hide()?;

    tracing::debug!(window = MAIN_WINDOW, "hidden");
    Ok(())
}

/// Toggles the main window, returning `true` if it is now visible.
///
/// "Visible" is not enough on its own: a window that is up but buried behind
/// another application should come forward, not vanish. So the window hides
/// only when it is both visible and focused — otherwise the toggle raises it.
pub fn toggle_main<R: Runtime>(app: &AppHandle<R>) -> AppResult<bool> {
    let window = main_window(app)?;

    let visible = window.is_visible().unwrap_or(false);
    let focused = window.is_focused().unwrap_or(false);

    if visible && focused {
        hide_main(app)?;
        Ok(false)
    } else {
        show_main(app)?;
        Ok(true)
    }
}

/// Toggles the main window. Mirrors what the tray does, for a UI affordance.
#[tauri::command]
pub fn window_toggle(app: AppHandle) -> AppResult<()> {
    toggle_main(&app).map(|_| ())
}

/// Hides the main window to the tray.
#[tauri::command]
pub fn window_hide(app: AppHandle) -> AppResult<()> {
    hide_main(&app)
}

/// Quits Aegis for real. The one shutdown path, shared by the tray menu and
/// the command below.
///
/// The flag is set *before* `exit`, because the exit tears the window down and
/// the close handler must be able to tell this apart from a user closing the
/// window (which only hides it).
pub fn quit<R: Runtime>(app: &AppHandle<R>) {
    match app.try_state::<AppState>() {
        Some(state) => {
            if state.begin_quit() {
                tracing::info!(uptime_s = state.uptime().as_secs(), "quit requested");
            }
        }
        // Only reachable if shutdown races setup. Exiting is still correct;
        // the close handler simply has nothing to consult.
        None => tracing::warn!("quit requested before state was managed"),
    }
    app.exit(0);
}

/// Quits Aegis.
#[tauri::command]
pub fn app_quit(app: AppHandle) -> AppResult<()> {
    quit(&app);
    Ok(())
}
