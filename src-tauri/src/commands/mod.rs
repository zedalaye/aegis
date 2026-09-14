//! IPC command surface: one module per command domain (AGENTS.md).
//!
//! Modules here hold only the `#[tauri::command]` entry points and the thin
//! glue around them. Real behaviour belongs to the runtime modules they call,
//! so a command stays readable as a permission-and-shape check.
//!
//! Domains land with their phases: `window`, `project`, `audit`, `session`,
//! `approval`, from Phase 8 `settings`, from Phase 11 `workspace` — the
//! shared-file convention inside a project's folder, plus `workspace_reveal`
//! which opens a contained path in the OS file manager (PLAN 7.10) — from Phase 12 `agent`,
//! the identities a session can be opened as, from Phase 13 `skill`, the
//! runbooks those identities may run, and from Phase 14 `memory` — what one
//! identity has learned, and the half of that a person owns, and from Phase 17
//! `board` — the structured read of a project's status, and the runs its audit
//! log folds into, and from Phase 18 `connector` — the external MCP servers
//! this installation runs, which is the one command domain that names a
//! program to start and is therefore the one with no tool behind it. And
//! `explorer` (PLAN 7.15): the read-only tree and preview of the open
//! project's folder, and the one write the operator makes from it — a dropped
//! file, copied in as a brief. And `roster` (PLAN 7.14): the preview of a
//! project's roster proposal, and the apply that creates the identities it
//! names — the grant, pressed by a person, with no tool behind it.
//!
//! Argument names are `snake_case` on the wire (PLAN 2). Tauri would otherwise
//! accept `camelCase` from JavaScript and rename it, so every command taking
//! arguments carries `#[tauri::command(rename_all = "snake_case")]` and the
//! TypeScript wrappers spell the Rust names.

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
