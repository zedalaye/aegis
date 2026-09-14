//! System tray icon and menu.
//!
//! Optional (PLAN 5.3): [`init`] returns a `Result` the caller logs.
//!
//! * **macOS**: click opens the menu (toggle first); template icon.
//! * **Windows**: left click toggles the window, right click opens the menu.
//! * **Linux**: AppIndicator only reports menu items, so the menu is everything.

use std::panic::{catch_unwind, AssertUnwindSafe};

use serde::Serialize;
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Runtime,
};
use ts_rs::TS;

use crate::commands::window::{self, MAIN_WINDOW};
use crate::error::{AppError, AppResult};

/// Identifier of the single tray icon.
pub const TRAY_ID: &str = "main";

const MENU_TOGGLE: &str = "tray_toggle";
const MENU_QUIT: &str = "tray_quit";

/// Emitted to the main window when the tray brings the app forward (PLAN 2.2).
const EVENT_TRAY_ACTIVATE: &str = "tray:activate";

/// Payload of [`EVENT_TRAY_ACTIVATE`].
///
/// `action` is `"show"`; PLAN 2.2's `"new_session"` has no menu item yet.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct TrayActivate {
    /// Why the window came forward.
    pub action: String,
}

/// Installs the tray icon.
///
/// Errors instead of panicking without a status area. The Linux AppIndicator
/// bindings panic when the library is missing (PLAN 5.3), so
/// [`catch_unwind`] turns that into an `Err`.
pub fn init<R: Runtime>(app: &AppHandle<R>) -> AppResult<()> {
    match catch_unwind(AssertUnwindSafe(|| install(app))) {
        Ok(result) => result,
        Err(_) => {
            tracing::warn!(
                "AppIndicator library missing or unusable; continuing without a tray icon"
            );
            Err(AppError::Internal {
                what: "the system tray could not be installed",
            })
        }
    }
}

fn install<R: Runtime>(app: &AppHandle<R>) -> AppResult<()> {
    let toggle = MenuItem::with_id(app, MENU_TOGGLE, "Show / Hide Aegis", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, MENU_QUIT, "Quit Aegis", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&toggle, &separator, &quit])?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("Aegis")
        .menu(&menu)
        .on_menu_event(on_menu_event)
        .on_tray_icon_event(on_tray_icon_event);

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }

    #[cfg(target_os = "macos")]
    {
        builder = builder.icon_as_template(true);
    }
    #[cfg(not(target_os = "macos"))]
    {
        builder = builder.show_menu_on_left_click(false);
    }

    builder.build(app)?;

    tracing::info!(tray = TRAY_ID, "tray installed");
    Ok(())
}

/// Menu selections. Identical on every platform.
fn on_menu_event<R: Runtime>(app: &AppHandle<R>, event: tauri::menu::MenuEvent) {
    match event.id().as_ref() {
        MENU_TOGGLE => toggle_and_announce(app),
        MENU_QUIT => window::quit(app),
        other => tracing::warn!(id = other, "unhandled tray menu id"),
    }
}

/// Direct clicks on the icon.
///
/// This fires on Windows only in practice: macOS hands left clicks to the
/// menu by convention, and AppIndicator on Linux never reports a click at
/// all. Both reach the same toggle through the menu item.
fn on_tray_icon_event<R: Runtime>(tray: &tauri::tray::TrayIcon<R>, event: TrayIconEvent) {
    if let TrayIconEvent::Click {
        button: MouseButton::Left,
        button_state: MouseButtonState::Up,
        ..
    } = event
    {
        toggle_and_announce(tray.app_handle());
    }
}

/// Toggles the window and, when that made it visible, tells the UI why it
/// woke up. A hide needs no announcement: nothing is listening.
fn toggle_and_announce<R: Runtime>(app: &AppHandle<R>) {
    match window::toggle_main(app) {
        Ok(true) => {
            if let Err(err) = app.emit_to(
                MAIN_WINDOW,
                EVENT_TRAY_ACTIVATE,
                TrayActivate {
                    action: "show".to_owned(),
                },
            ) {
                tracing::warn!(%err, "could not emit {EVENT_TRAY_ACTIVATE}");
            }
        }
        Ok(false) => {}
        Err(err) => tracing::warn!(%err, "tray could not toggle the main window"),
    }
}
