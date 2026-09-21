use serde_json::json;

use super::*;

fn models(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|id| (*id).to_owned()).collect()
}

#[test]
fn per_token_dollars_become_micro_dollars_per_million() {
    assert_eq!(per_million(&json!("0.000003")), Some(3_000_000));
    assert_eq!(per_million(&json!(3e-6)), Some(3_000_000));
    assert_eq!(per_million(&json!(1.5e-5)), Some(15_000_000));
    assert_eq!(per_million(&json!("0.0000003")), Some(300_000));
    assert_eq!(per_million(&json!("0")), Some(0));
    assert_eq!(per_million(&json!("-1")), None, "varies by route, not free");
    assert_eq!(per_million(&json!("n/a")), None);
    assert_eq!(per_million(&json!(null)), None);
}

#[test]
fn an_openrouter_catalog_prices_its_own_models() {
    let listing = json!({ "data": [{
        "id": "anthropic/claude-sonnet-4.5",
        "pricing": {
            "prompt": "0.000003",
            "completion": "0.000015",
            "input_cache_read": "0.0000003",
            "input_cache_write": "0.00000375"
        }
    }, {
        "id": "openrouter/auto",
        "pricing": { "prompt": "-1", "completion": "-1" }
    }]});

    let price = catalog_price(&listing, "anthropic/claude-sonnet-4.5").expect("priced");
    assert_eq!(price.input, 3_000_000);
    assert_eq!(price.output, 15_000_000);
    assert_eq!(price.cache_read, Some(300_000));
    assert_eq!(price.cache_write, Some(3_750_000));
    assert_eq!(catalog_price(&listing, "openrouter/auto"), None);
    assert_eq!(catalog_price(&listing, "absent"), None);
}

#[test]
fn litellm_is_matched_by_id_or_vendor_prefix_only() {
    let table = json!({
        "claude-sonnet-4-5": {
            "input_cost_per_token": 3e-6,
            "output_cost_per_token": 1.5e-5,
            "cache_read_input_token_cost": 3e-7
        },
        "gemini/gemini-2.5-pro": {
            "input_cost_per_token": 1.25e-6,
            "output_cost_per_token": 1e-5
        },
        "gpt-4o-2024-08-06": {
            "input_cost_per_token": 2.5e-6,
            "output_cost_per_token": 1e-5
        }
    });

    let (price, key) =
        litellm_price(&table, AuthKind::ClaudeCli, "claude-sonnet-4-5").expect("bare id");
    assert_eq!(key, "claude-sonnet-4-5");
    assert_eq!(price.cache_read, Some(300_000));
    assert_eq!(price.cache_write, None, "blank charges the input price");

    let (_, key) =
        litellm_price(&table, AuthKind::Gemini, "models/gemini-2.5-pro").expect("prefixed");
    assert_eq!(key, "gemini/gemini-2.5-pro");

    let (_, key) = litellm_price(&table, AuthKind::ApiKey, "anthropic/claude-sonnet-4-5")
        .expect("an OpenRouter-style id, bare in the table");
    assert_eq!(key, "claude-sonnet-4-5");

    assert_eq!(
        litellm_price(&table, AuthKind::ApiKey, "gpt-4o"),
        None,
        "a dated id is not the same model"
    );
}

#[test]
fn the_catalog_wins_and_the_rest_is_named() {
    let catalog_hit = ModelPrice {
        model: "a".to_owned(),
        input: 1,
        output: 2,
        cache_read: None,
        cache_write: None,
    };
    let table = json!({
        "a": { "input_cost_per_token": 9e-6, "output_cost_per_token": 9e-6 },
        "b": { "input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6 }
    });

    let suggestion = assemble(
        AuthKind::ApiKey,
        &models(&["a", "b", "c"]),
        vec![Some(catalog_hit.clone()), None, None],
        Some(&table),
        Vec::new(),
    );
    assert_eq!(suggestion.prices.len(), 2);
    assert_eq!(suggestion.prices[0].price, catalog_hit);
    assert_eq!(suggestion.prices[0].source, PriceSource::Catalog);
    assert_eq!(suggestion.prices[1].source, PriceSource::Litellm);
    assert_eq!(suggestion.prices[1].matched, None, "found under its own id");
    assert_eq!(suggestion.missing, ["c"]);
}
