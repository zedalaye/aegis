//! IPC command surface: one module per command domain (AGENTS.md).
//!
//! Only `#[tauri::command]` entry points and thin glue; behaviour lives in the
//! runtime modules. The full list is PLAN 2.1.
//!
//! Arguments are `snake_case` on the wire: every command with arguments uses
//! `#[tauri::command(rename_all = "snake_case")]`.

pub mod agent;
pub mod approval;
pub mod audit;
pub mod board;
pub mod connector;
pub mod explorer;
pub mod memory;
pub mod project;
pub mod roster;
pub mod routine;
pub mod session;
pub mod settings;
pub mod skill;
pub mod window;
pub mod workspace;
