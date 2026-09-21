//! Model spend, as the window reads it (PLAN 7.26).
//!
//! Read-only: the ledger is written by the turn loop and nothing else, and a
//! cap is set on its routine or identity, never here.

use tauri::State;

use crate::error::AppResult;
use crate::state::AppState;
use crate::store::SpendToday;

/// What each routine and each identity has spent today, UTC.
#[tauri::command]
pub fn spend_today(state: State<'_, AppState>) -> AppResult<SpendToday> {
    Ok(state.spend().today())
}
