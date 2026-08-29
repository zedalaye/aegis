//! IPC command surface: one module per command domain (AGENTS.md).
//!
//! Modules here hold only the `#[tauri::command]` entry points and the thin
//! glue around them. Real behaviour belongs to the runtime modules they call,
//! so a command stays readable as a permission-and-shape check.
//!
//! Domains land with their phases: `window`, `project`, `audit` and `session`
//! now, then `approval` and `settings`.
//!
//! Argument names are `snake_case` on the wire (PLAN 2). Tauri would otherwise
//! accept `camelCase` from JavaScript and rename it, so every command taking
//! arguments carries `#[tauri::command(rename_all = "snake_case")]` and the
//! TypeScript wrappers spell the Rust names.

pub mod audit;
pub mod project;
pub mod session;
pub mod window;
