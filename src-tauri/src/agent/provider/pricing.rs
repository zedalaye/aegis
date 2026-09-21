//! Suggested model prices (PLAN 7.26).
//!
//! Two places publish what a model costs; none of the vendors' own model APIs
//! do. In order:
//!
//! * **The row's own catalog**, where it carries OpenRouter's `pricing` object:
//!   dollars per token, as decimal strings.
//! * **LiteLLM's public table** ([`LITELLM_URL`]): dollars per token, as
//!   numbers, keyed by model id with an optional vendor prefix.
//!
//! A suggestion is never stored here. It fills the form; the operator saves.
//! Matching is by the id, and the id with a vendor prefix added or dropped —
//! nothing looser, since a near match is a wrong price.

use std::time::Duration;

use reqwest::Client;
use serde::Serialize;
use serde_json::{Map, Value};
use ts_rs::TS;

use super::catalog;
use crate::secrets::ApiKey;
use crate::store::{AuthKind, ModelPrice};

/// LiteLLM's price table. Fetched with a plain `GET`: no key, no header of
/// ours beyond the client's.
pub const LITELLM_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";

/// How long either source is waited for. A button is waiting.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Where a suggested price was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum PriceSource {
    /// The provider row's own model catalog.
    Catalog,
    /// LiteLLM's public table.
    Litellm,
}

/// One suggested price and where it came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct SuggestedPrice {
    /// The price, under the model id that was asked about.
    pub price: ModelPrice,
    /// Where it was read.
    pub source: PriceSource,
    /// The id it was found under, when not the one asked about
    /// (`anthropic/claude-…`, `gemini/…`).
    pub matched: Option<String>,
}

/// What [`suggest`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct PriceSuggestion {
    /// One per model found, in the order asked.
    pub prices: Vec<SuggestedPrice>,
    /// The models neither source knows.
    pub missing: Vec<String>,
    /// Why a source could not be read, one sentence each. Empty when both
    /// answered or were not needed.
    pub notes: Vec<String>,
}

/// Suggests prices for `models` on a row of `kind` at `base_url`.
pub async fn suggest(
    kind: AuthKind,
    base_url: &str,
    models: &[String],
    client: Option<&Client>,
    api_key: Option<&ApiKey>,
) -> PriceSuggestion {
    let mut notes = Vec::new();

    let listing = match catalog::listing(kind, base_url, client, api_key, TIMEOUT).await {
        Ok(listing) => Some(listing),
        Err(reason) => {
            tracing::debug!(%reason, "no model catalog to read prices from");
            None
        }
    };

    let from_catalog: Vec<Option<ModelPrice>> = models
        .iter()
        .map(|model| listing.as_ref().and_then(|body| catalog_price(body, model)))
        .collect();

    // Only asked for when the catalog left something out.
    let table = if from_catalog.iter().any(Option::is_none) {
        match fetch_litellm(client).await {
            Ok(table) => Some(table),
            Err(reason) => {
                notes.push(format!("LiteLLM's price table could not be read: {reason}"));
                None
            }
        }
    } else {
        None
    };

    assemble(kind, models, from_catalog, table.as_ref(), notes)
}

/// The pure half of [`suggest`]: what each source said, in order.
fn assemble(
    kind: AuthKind,
    models: &[String],
    from_catalog: Vec<Option<ModelPrice>>,
    table: Option<&Value>,
    notes: Vec<String>,
) -> PriceSuggestion {
    let mut out = PriceSuggestion {
        notes,
        ..PriceSuggestion::default()
    };
    for (model, found) in models.iter().zip(from_catalog) {
        if let Some(price) = found {
            out.prices.push(SuggestedPrice {
                price,
                source: PriceSource::Catalog,
                matched: None,
            });
            continue;
        }
        match table.and_then(|table| litellm_price(table, kind, model)) {
            Some((price, key)) => out.prices.push(SuggestedPrice {
                matched: (key != *model).then_some(key),
                price,
                source: PriceSource::Litellm,
            }),
            None => out.missing.push(model.clone()),
        }
    }
    out
}

async fn fetch_litellm(client: Option<&Client>) -> Result<Value, String> {
    let http = client.ok_or_else(|| "no HTTP client in this process".to_owned())?;
    let response = http
        .get(LITELLM_URL)
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|err| format!("GitHub could not be reached ({err})"))?;
    if !response.status().is_success() {
        return Err(format!("GitHub answered {}", response.status()));
    }
    response
        .json()
        .await
        .map_err(|_| "the table is not JSON".to_owned())
}

/// A price from OpenRouter's `pricing` object on `model`'s catalog entry.
///
/// A negative figure is OpenRouter's "varies by route": no price, not free.
fn catalog_price(listing: &Value, model: &str) -> Option<ModelPrice> {
    let pricing = catalog::find_entry(listing, model)?
        .get("pricing")?
        .as_object()?;
    Some(ModelPrice {
        model: model.to_owned(),
        input: per_million(pricing.get("prompt")?)?,
        output: per_million(pricing.get("completion")?)?,
        cache_read: pricing.get("input_cache_read").and_then(per_million),
        cache_write: pricing.get("input_cache_write").and_then(per_million),
    })
}

/// The ids LiteLLM may list `model` under, most specific first.
fn litellm_keys(kind: AuthKind, model: &str) -> Vec<String> {
    let bare = model.strip_prefix("models/").unwrap_or(model);
    let mut keys = vec![bare.to_owned()];
    let vendor = match kind {
        AuthKind::ClaudeCli => Some("anthropic"),
        AuthKind::CodexCli => Some("openai"),
        AuthKind::Gemini => Some("gemini"),
        AuthKind::GrokCli => Some("xai"),
        AuthKind::ApiKey => None,
    };
    if let Some(vendor) = vendor {
        keys.push(format!("{vendor}/{bare}"));
    }
    // An OpenRouter-style id: `vendor/model`, listed bare or under `openrouter/`.
    if let Some((_, name)) = bare.split_once('/') {
        keys.push(name.to_owned());
        keys.push(format!("openrouter/{bare}"));
    }
    keys
}

/// A price from LiteLLM's table, and the key it was found under.
fn litellm_price(table: &Value, kind: AuthKind, model: &str) -> Option<(ModelPrice, String)> {
    let table = table.as_object()?;
    litellm_keys(kind, model).into_iter().find_map(|key| {
        let entry: &Map<String, Value> = table.get(&key)?.as_object()?;
        let price = ModelPrice {
            model: model.to_owned(),
            input: per_million(entry.get("input_cost_per_token")?)?,
            output: per_million(entry.get("output_cost_per_token")?)?,
            cache_read: entry
                .get("cache_read_input_token_cost")
                .and_then(per_million),
            cache_write: entry
                .get("cache_creation_input_token_cost")
                .and_then(per_million),
        };
        Some((price, key))
    })
}

/// Dollars per token — a number or a decimal string — as micro-dollars per
/// million tokens, rounded to the nearest. `None` for a negative or
/// unreadable figure.
fn per_million(value: &Value) -> Option<u64> {
    let dollars = match value {
        Value::Number(number) => number.as_f64()?,
        Value::String(text) => text.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    if !dollars.is_finite() || dollars < 0.0 {
        return None;
    }
    // Per token to per million (×1e6), dollars to micro-dollars (×1e6).
    let micros = (dollars * 1e12).round();
    // A suggestion over the stored ceiling is refused on save, not here.
    (micros <= 1e15).then_some(micros as u64)
}

#[cfg(test)]
mod tests;
