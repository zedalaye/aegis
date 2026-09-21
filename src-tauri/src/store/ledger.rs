//! The spend ledger: `ledger.json` (PLAN 7.26).
//!
//! Harness-owned: the turn loop writes it and no command or tool does. One
//! row per turn, updated after every round, so a run that is going now is
//! counted by the next check of any other run.
//!
//! * **Money is whole micro-dollars** ([`Micros`]); no float is ever summed.
//! * **A day is UTC**, like a routine's `runs_per_day`.
//! * **Rows older than [`RETENTION_DAYS`] are pruned** on every write.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use chrono::{NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{now, quarantine, strip_bom, write_atomic};
use crate::error::{AppError, AppResult};

/// Name of the document under the application-data directory.
const LEDGER_FILE: &str = "ledger.json";

/// Schema version of [`LedgerFile`].
const SCHEMA_VERSION: u32 = 1;

/// How long a row is kept: two months, for a month's view with its previous.
pub const RETENTION_DAYS: i64 = 62;

/// Micro-dollars: 1 $ is `1_000_000`.
pub type Micros = u64;

/// One dollar, in [`Micros`].
pub const DOLLAR: Micros = 1_000_000;

/// The highest price per million tokens a row accepts: a typo guard, not a
/// market view.
pub const PRICE_MAX: Micros = 1_000 * DOLLAR;

/// The highest cap: past this a cap is not a ceiling anybody meant.
pub const CAP_MAX: Micros = 10_000 * DOLLAR;

// ---------------------------------------------------------------------------
// Prices and caps
// ---------------------------------------------------------------------------

/// What one model costs on one provider row, per million tokens.
///
/// The operator's word: typed, or accepted from a suggestion
/// ([`pricing`](crate::agent::provider::pricing)) and saved. Nothing stores a
/// price the operator did not save, and nothing reads one from the workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct ModelPrice {
    /// The model id, exactly as the row or an identity sends it.
    pub model: String,
    /// Prompt tokens not served from the cache.
    #[ts(type = "number")]
    pub input: Micros,
    /// Reply tokens.
    #[ts(type = "number")]
    pub output: Micros,
    /// Prompt tokens read from the cache. `None` charges [`Self::input`].
    #[serde(default)]
    #[ts(type = "number | null")]
    pub cache_read: Option<Micros>,
    /// Prompt tokens written to the cache. `None` charges [`Self::input`].
    #[serde(default)]
    #[ts(type = "number | null")]
    pub cache_write: Option<Micros>,
}

impl ModelPrice {
    /// What these token counts cost, rounded up to the micro-dollar.
    ///
    /// `prompt` includes the cached tokens, as every provider reports it.
    pub fn cost(&self, prompt: u64, cache_read: u64, cache_write: u64, completion: u64) -> Micros {
        let uncached = prompt.saturating_sub(cache_read.saturating_add(cache_write));
        let per_million = u128::from(uncached) * u128::from(self.input)
            + u128::from(cache_read) * u128::from(self.cache_read.unwrap_or(self.input))
            + u128::from(cache_write) * u128::from(self.cache_write.unwrap_or(self.input))
            + u128::from(completion) * u128::from(self.output);
        let micros = per_million.div_ceil(1_000_000);
        Micros::try_from(micros).unwrap_or(Micros::MAX)
    }
}

/// A routine's or an identity's model-spend ceilings. `None` is no cap.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct SpendCaps {
    /// Most one run may spend: a routine's run or a brief's session, or a
    /// turn in a session someone opened.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub per_run: Option<Micros>,
    /// Most one UTC day may spend.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub per_day: Option<Micros>,
}

impl SpendCaps {
    /// Whether any cap is set.
    pub const fn any(&self) -> bool {
        self.per_run.is_some() || self.per_day.is_some()
    }

    /// Checks both caps: above zero, at most [`CAP_MAX`], and a run no
    /// larger than its day.
    pub fn check(&self) -> Result<Self, String> {
        for cap in [self.per_run, self.per_day].into_iter().flatten() {
            if cap == 0 {
                return Err(
                    "a cap of zero stops everything — leave it blank for no cap, or pause \
                            instead"
                        .to_owned(),
                );
            }
            if cap > CAP_MAX {
                return Err(format!(
                    "a cap is at most {}; past that it is not a ceiling",
                    dollars(CAP_MAX)
                ));
            }
        }
        if let (Some(run), Some(day)) = (self.per_run, self.per_day) {
            if run > day {
                return Err(format!(
                    "a run may not be allowed more ({}) than the whole day ({})",
                    dollars(run),
                    dollars(day)
                ));
            }
        }
        Ok(*self)
    }
}

/// Micro-dollars as `$0.50`: two decimals, or more when the amount needs them.
pub fn dollars(micros: Micros) -> String {
    let whole = micros / DOLLAR;
    let frac = micros % DOLLAR;
    if frac % 10_000 == 0 {
        format!("${whole}.{:02}", frac / 10_000)
    } else {
        let digits = format!("{frac:06}");
        format!("${whole}.{}", digits.trim_end_matches('0'))
    }
}

/// Checks a row's price list: named models, each once, within [`PRICE_MAX`].
pub fn check_prices(prices: &[ModelPrice]) -> Result<Vec<ModelPrice>, String> {
    let mut out: Vec<ModelPrice> = Vec::with_capacity(prices.len());
    for price in prices {
        let model = price.model.trim();
        if model.is_empty() {
            return Err("a price names the model it is for".to_owned());
        }
        if out.iter().any(|kept| kept.model == model) {
            return Err(format!("`{model}` is priced twice"));
        }
        let all = [
            Some(price.input),
            Some(price.output),
            price.cache_read,
            price.cache_write,
        ];
        if all.into_iter().flatten().any(|each| each > PRICE_MAX) {
            return Err(format!(
                "`{model}` costs more than {} per million tokens, which is a typo",
                dollars(PRICE_MAX)
            ));
        }
        out.push(ModelPrice {
            model: model.to_owned(),
            ..price.clone()
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

/// What one turn spent, and on whose account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpendRow {
    /// The turn. The key: one row each.
    pub turn_id: String,
    /// Its session: a routine's or a brief's run.
    pub session_id: String,
    /// The project it ran in; empty for a session with none.
    pub project_id: String,
    /// The identity it ran as.
    pub agent_id: String,
    /// The routine whose run it was; empty otherwise.
    #[serde(default)]
    pub routine_id: String,
    /// The provider row that answered.
    pub provider_id: String,
    /// The model it sent.
    pub model: String,
    /// The UTC date the turn started, `2026-09-21`.
    pub day: String,
    /// RFC3339 UTC, when it was last charged.
    pub at: String,
    /// What it cost.
    pub micros: Micros,
    /// Whether a round reported no usage and was estimated.
    #[serde(default)]
    pub estimated: bool,
}

/// Who a charge is for: every field of a [`SpendRow`] except the amount.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account<'a> {
    /// The turn.
    pub turn_id: &'a str,
    /// Its session.
    pub session_id: &'a str,
    /// Its project.
    pub project_id: &'a str,
    /// The identity.
    pub agent_id: &'a str,
    /// The routine, or empty.
    pub routine_id: &'a str,
    /// The provider row.
    pub provider_id: &'a str,
    /// The model.
    pub model: &'a str,
}

/// Which rows a sum covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope<'a> {
    /// One turn.
    Turn(&'a str),
    /// One session: a routine's or a brief's whole run.
    Session(&'a str),
    /// A routine, on one UTC day.
    RoutineDay(&'a str, &'a str),
    /// An identity, on one UTC day.
    AgentDay(&'a str, &'a str),
}

impl Scope<'_> {
    fn holds(&self, row: &SpendRow) -> bool {
        match *self {
            Self::Turn(id) => row.turn_id == id,
            Self::Session(id) => row.session_id == id,
            Self::RoutineDay(id, day) => row.routine_id == id && row.day == day,
            Self::AgentDay(id, day) => row.agent_id == id && row.day == day,
        }
    }
}

/// Today, UTC, as `2026-09-21`.
pub fn today() -> String {
    Utc::now().format("%Y-%m-%d").to_string()
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// The document itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LedgerFile {
    version: u32,
    rows: Vec<SpendRow>,
}

/// The ledger: one mutex over the rows, written out on every charge.
#[derive(Debug)]
pub struct SpendLedger {
    path: PathBuf,
    rows: Mutex<Vec<SpendRow>>,
}

impl SpendLedger {
    /// Loads the ledger from `data_dir`. Never fails: a damaged document is
    /// moved aside and the ledger starts empty — which undercounts today, and
    /// is logged at `error` for that reason.
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(LEDGER_FILE);

        let rows = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<LedgerFile>(strip_bom(&bytes)) {
                Ok(file) if file.version == SCHEMA_VERSION => file.rows,
                Ok(file) => {
                    tracing::error!(
                        found = file.version,
                        expected = SCHEMA_VERSION,
                        "unknown ledger version; today's spend starts from zero"
                    );
                    quarantine(&path);
                    Vec::new()
                }
                Err(err) => {
                    tracing::error!(%err, "the ledger is not readable JSON; today's spend starts from zero");
                    quarantine(&path);
                    Vec::new()
                }
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(err) => {
                tracing::error!(%err, "could not read the ledger; today's spend starts from zero");
                Vec::new()
            }
        };

        Self {
            path,
            rows: Mutex::new(rows),
        }
    }

    /// Locks the rows, recovering from poison: a charge cannot leave them torn.
    fn rows(&self) -> MutexGuard<'_, Vec<SpendRow>> {
        self.rows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Adds `micros` to the turn's row, opening it on the first charge.
    ///
    /// Kept in memory even when the write fails, so enforcement goes on
    /// counting; the error is the caller's to log.
    pub fn charge(&self, account: &Account<'_>, micros: Micros, estimated: bool) -> AppResult<()> {
        let mut rows = self.rows();
        let stamp = now();

        match rows.iter_mut().find(|row| row.turn_id == account.turn_id) {
            Some(row) => {
                row.micros = row.micros.saturating_add(micros);
                row.estimated |= estimated;
                row.at = stamp;
            }
            None => rows.push(SpendRow {
                turn_id: account.turn_id.to_owned(),
                session_id: account.session_id.to_owned(),
                project_id: account.project_id.to_owned(),
                agent_id: account.agent_id.to_owned(),
                routine_id: account.routine_id.to_owned(),
                provider_id: account.provider_id.to_owned(),
                model: account.model.to_owned(),
                day: today(),
                at: stamp,
                micros,
                estimated,
            }),
        }

        prune(&mut rows, Utc::now().date_naive());
        self.save(&rows)
    }

    /// What the rows in `scope` spent.
    pub fn spent(&self, scope: Scope<'_>) -> Micros {
        self.rows()
            .iter()
            .filter(|row| scope.holds(row))
            .fold(0, |sum, row| sum.saturating_add(row.micros))
    }

    /// What each routine and each identity spent today.
    pub fn today(&self) -> SpendToday {
        let day = today();
        let mut out = SpendToday::default();
        for row in self.rows().iter().filter(|row| row.day == day) {
            if !row.routine_id.is_empty() {
                let sum = out.routines.entry(row.routine_id.clone()).or_default();
                *sum = sum.saturating_add(row.micros);
            }
            let sum = out.agents.entry(row.agent_id.clone()).or_default();
            *sum = sum.saturating_add(row.micros);
        }
        out
    }

    /// Serializes the rows and replaces the document atomically. Compact:
    /// the ledger is rewritten every round and is not meant to be edited.
    fn save(&self, rows: &[SpendRow]) -> AppResult<()> {
        let file = LedgerFile {
            version: SCHEMA_VERSION,
            rows: rows.to_vec(),
        };
        let bytes = serde_json::to_vec(&file).map_err(|err| AppError::Store {
            action: "serialize",
            source: io::Error::other(err),
        })?;
        write_atomic(&self.path, &bytes).map_err(|err| AppError::Store {
            action: "save",
            source: err,
        })
    }
}

/// Drops rows older than [`RETENTION_DAYS`]. A row whose day does not parse is
/// kept: it is somebody's money, and a bad date is not a reason to forget it.
fn prune(rows: &mut Vec<SpendRow>, today: NaiveDate) {
    let Some(oldest) = today.checked_sub_signed(chrono::TimeDelta::days(RETENTION_DAYS)) else {
        return;
    };
    rows.retain(|row| {
        NaiveDate::parse_from_str(&row.day, "%Y-%m-%d").map_or(true, |day| day >= oldest)
    });
}

/// Today's model spend, by routine and by identity (PLAN 7.26).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct SpendToday {
    /// Routine id to micro-dollars.
    #[ts(type = "Record<string, number>")]
    pub routines: std::collections::BTreeMap<String, Micros>,
    /// Identity id to micro-dollars.
    #[ts(type = "Record<string, number>")]
    pub agents: std::collections::BTreeMap<String, Micros>,
}

#[cfg(test)]
mod tests;
