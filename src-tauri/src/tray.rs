//! System tray icon and menu.
//!
//! The tray is a convenience, never a requirement (PLAN 5.3): GNOME shows
//! nothing without the AppIndicator extension, and a headless or minimal WM
//! may have no status area at all. [`init`] therefore returns a `Result` that
//! the caller logs and moves past — the window stays the primary surface.
//!
//! Platform conventions differ and are honoured rather than averaged:
//!
//! * **macOS** — an `NSStatusItem` opens its menu on click; the window toggle
//!   is the first menu item. The icon is flagged as a template so the system
//!   tints it for the light and dark menu bar.
//! * **Windows** — left click toggles the window directly, right click opens
//!   the menu.
//! * **Linux** — AppIndicator reports menu activations and nothing else, so
//!   the menu is the entire interface and `show_menu_on_left_click` is inert.
//!   The toggle item carries the interaction there.

use serde::Serialize;
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Runtime,
};

use crate::commands::window::{self, MAIN_WINDOW};
use crate::error::AppResult;

/// Identifier of the single tray icon.
pub const TRAY_ID: &str = "main";

const MENU_TOGGLE: &str = "tray_toggle";
const MENU_QUIT: &str = "tray_quit";

/// Emitted to the main window when the tray brings the app forward (PLAN 2.2).
const EVENT_TRAY_ACTIVATE: &str = "tray:activate";

/// Payload of [`EVENT_TRAY_ACTIVATE`].
///
/// `action` is `"show"` today; Phase 5 adds `"new_session"` once there are
/// sessions for the tray to create.
#[derive(Debug, Clone, Serialize)]
struct TrayActivate {
    action: &'static str,
}

/// Installs the tray icon.
///
/// Returns an error instead of panicking when the platform has no usable
/// status area — losing the tray degrades the app, it must not stop it.
pub fn init<R: Runtime>(app: &AppHandle<R>) -> AppResult<()> {
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
                TrayActivate { action: "show" },
            ) {
                tracing::warn!(%err, "could not emit {EVENT_TRAY_ACTIVATE}");
            }
        }
        Ok(false) => {}
        Err(err) => tracing::warn!(%err, "tray could not toggle the main window"),
    }
}
