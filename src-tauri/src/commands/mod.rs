//! IPC command surface: one module per command domain (AGENTS.md).
//!
//! Modules here hold only the `#[tauri::command]` entry points and the thin
//! glue around them. Real behaviour belongs to the runtime modules they call,
//! so a command stays readable as a permission-and-shape check.
//!
//! Domains land with their phases: `window` now, then `project`, `session`,
//! `approval`, `settings` and `audit`.

pub mod window;
