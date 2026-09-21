use super::*;

use tempfile::TempDir;

#[test]
fn a_trailing_slash_is_removed_so_requests_carry_one() {
    assert_eq!(
        normalize_base_url("https://api.openai.com/v1/").expect("accepted"),
        "https://api.openai.com/v1"
    );
    assert_eq!(
        normalize_base_url("  https://api.openai.com/v1  ").expect("accepted"),
        "https://api.openai.com/v1"
    );
}

/// Emptying the field is how a user goes back to the scripted provider, so
/// it is an answer rather than a validation failure.
#[test]
fn an_empty_base_url_is_accepted_as_unset() {
    assert_eq!(normalize_base_url("").expect("accepted"), "");
    assert_eq!(normalize_base_url("   ").expect("accepted"), "");
}

#[test]
fn a_local_server_over_plain_http_is_allowed() {
    assert_eq!(
        normalize_base_url("http://127.0.0.1:11434/v1").expect("accepted"),
        "http://127.0.0.1:11434/v1"
    );
}

/// The failures worth naming, each carrying the message the user needs
/// rather than the parser's.
#[test]
fn a_base_url_that_cannot_work_is_refused_with_a_reason() {
    let cases = [
        ("api.openai.com/v1", "should look like"),
        ("ftp://example.com", "not a scheme"),
        ("https://", "should look like"),
        (
            "https://api.openai.com/v1/chat/completions",
            "appends that itself",
        ),
    ];

    for (input, expected) in cases {
        let err = normalize_base_url(input).expect_err(input).to_string();
        assert!(err.contains(expected), "{input} gave `{err}`");
    }
}

#[test]
fn a_model_id_is_trimmed_and_must_be_one_word() {
    assert_eq!(
        normalize_model("  gpt-4o-mini \n").expect("accepted"),
        "gpt-4o-mini"
    );
    assert!(normalize_model("gpt 4o mini").is_err());
}

/// The key is the one thing that must never be in this file. A user who
/// opens `settings.json` should find nothing worth protecting.
#[test]
fn the_document_never_contains_a_secret() {
    let dir = TempDir::new().expect("temp dir");
    let store = SettingsStore::load(dir.path());

    store
        .set(
            "https://api.openai.com/v1",
            "gpt-4o-mini",
            AuthKind::ApiKey,
            None,
        )
        .expect("accepted");

    let written = fs::read_to_string(store.path()).expect("the document");
    assert!(written.contains("api.openai.com"));
    assert!(written.contains("gpt-4o-mini"));
    assert!(
        !written.contains("sk-") && !written.contains("oat01"),
        "the settings document contains a secret: {written}"
    );
}

#[test]
fn settings_survive_a_restart() {
    let dir = TempDir::new().expect("temp dir");

    SettingsStore::load(dir.path())
        .set(
            "https://example.test/v1",
            "some-model",
            AuthKind::ApiKey,
            None,
        )
        .expect("accepted");

    let reopened = SettingsStore::load(dir.path()).get();
    assert_eq!(reopened.base_url, "https://example.test/v1");
    assert_eq!(reopened.model, "some-model");
    assert!(reopened.is_configured());
}

/// A rejected value must not take the accepted one with it: the user is
/// mid-correction, and losing the other field is how a settings panel
/// makes a typo expensive.
#[test]
fn a_rejected_change_leaves_the_previous_settings_intact() {
    let dir = TempDir::new().expect("temp dir");
    let store = SettingsStore::load(dir.path());

    store
        .set(
            "https://example.test/v1",
            "some-model",
            AuthKind::ApiKey,
            None,
        )
        .expect("accepted");
    store
        .set("not a url", "some-model", AuthKind::ApiKey, None)
        .expect_err("refused");

    assert_eq!(store.get().base_url, "https://example.test/v1");
}

#[test]
fn a_provider_is_configured_only_once_both_fields_are_set() {
    let mut provider = ProviderSettings::default();
    assert!(!provider.is_configured(), "a fresh install");

    provider.base_url = "https://api.openai.com/v1".to_owned();
    assert!(!provider.is_configured(), "a URL with no model");

    provider.model = "gpt-4o-mini".to_owned();
    assert!(provider.is_configured());
}

#[test]
fn each_cli_kind_has_a_https_default_url() {
    for kind in [AuthKind::ClaudeCli, AuthKind::CodexCli, AuthKind::GrokCli] {
        let url = kind.default_base_url();
        assert!(url.starts_with("https://"), "{kind:?} defaulted to {url}");
        assert!(!kind.default_model().is_empty(), "{kind:?} has no model");
    }
    assert_eq!(AuthKind::presets().len(), 5);
    assert_eq!(
        AuthKind::Gemini.default_base_url(),
        "https://generativelanguage.googleapis.com/v1beta"
    );
    assert_eq!(AuthKind::Gemini.default_model(), "gemini-2.5-flash");
    assert!(!AuthKind::Gemini.is_cli());
}

#[test]
fn a_cli_login_is_configured_with_a_model_alone() {
    let provider = ProviderSettings {
        auth_kind: AuthKind::ClaudeCli,
        base_url: String::new(),
        model: "claude-sonnet-4-6".to_owned(),
        max_output_tokens: None,
        prices: Vec::new(),
    };
    assert!(provider.is_configured());
}

#[test]
fn gemini_is_configured_with_a_model_alone() {
    let provider = ProviderSettings {
        auth_kind: AuthKind::Gemini,
        base_url: String::new(),
        model: "gemini-2.5-flash".to_owned(),
        max_output_tokens: None,
        prices: Vec::new(),
    };
    assert!(provider.is_configured());
    assert!(!provider.auth_kind.is_cli());
}

#[test]
fn an_old_document_without_auth_kind_is_an_api_key() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join(SETTINGS_FILE);
    fs::write(
        &path,
        br#"{"version":1,"provider":{"base_url":"https://x.test/v1","model":"m"}}"#,
    )
    .expect("write");

    let loaded = SettingsStore::load(dir.path()).get();
    assert_eq!(loaded.auth_kind, AuthKind::ApiKey);
    assert_eq!(loaded.base_url, "https://x.test/v1");
    assert!(loaded.is_configured());
}

/// A document from a future version is quarantined rather than guessed at,
/// and the app still starts.
#[test]
fn an_unknown_version_is_quarantined() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join(SETTINGS_FILE);
    fs::write(
        &path,
        br#"{"version":99,"provider":{"base_url":"https://x.test","model":"m"}}"#,
    )
    .expect("write");

    let store = SettingsStore::load(dir.path());

    assert_eq!(store.get(), ProviderSettings::default());
    assert!(!path.exists(), "the damaged document was moved aside");
}

fn entry(id: &str, model: &str, cap: Option<u32>) -> ProviderEntry {
    ProviderEntry {
        id: id.to_owned(),
        label: String::new(),
        settings: ProviderSettings {
            auth_kind: AuthKind::ApiKey,
            base_url: format!("https://{id}.test/v1"),
            model: model.to_owned(),
            max_output_tokens: cap,
            prices: Vec::new(),
        },
    }
}

/// The resolution order PLAN 7.19 fixes, one row per case.
#[test]
fn a_binding_resolves_session_then_identity_then_row() {
    let rows = [
        entry(DEFAULT_PROVIDER_ID, "d-model", Some(100)),
        entry("second", "s-model", Some(200)),
    ];
    let identity = |provider: &'static str, model: &'static str| BindingRequest {
        identity_provider: provider,
        identity_model: model,
        ..BindingRequest::default()
    };

    let cases: [(&str, BindingRequest<'_>, &str, &str, Option<u32>); 8] = [
        (
            "identity default",
            identity(DEFAULT_PROVIDER_ID, ""),
            DEFAULT_PROVIDER_ID,
            "d-model",
            Some(100),
        ),
        (
            "identity row, row model",
            identity("second", ""),
            "second",
            "s-model",
            Some(200),
        ),
        (
            "identity model",
            identity("second", "s-big"),
            "second",
            "s-big",
            None,
        ),
        (
            "session model only",
            BindingRequest {
                session_model: Some("s-small"),
                ..identity("second", "s-big")
            },
            "second",
            "s-small",
            None,
        ),
        (
            "session provider only",
            BindingRequest {
                session_provider: Some(DEFAULT_PROVIDER_ID),
                ..identity("second", "s-big")
            },
            DEFAULT_PROVIDER_ID,
            "d-model",
            Some(100),
        ),
        (
            "session pair",
            BindingRequest {
                session_provider: Some(DEFAULT_PROVIDER_ID),
                session_model: Some("d-other"),
                ..identity("second", "s-big")
            },
            DEFAULT_PROVIDER_ID,
            "d-other",
            None,
        ),
        (
            "missing row",
            identity("gone", "g-model"),
            DEFAULT_PROVIDER_ID,
            "d-model",
            Some(100),
        ),
        (
            "session model equal to the row's keeps the ceiling",
            BindingRequest {
                session_model: Some("s-model"),
                ..identity("second", "s-big")
            },
            "second",
            "s-model",
            Some(200),
        ),
    ];

    for (name, request, provider, model, cap) in cases {
        let binding = resolve(&rows, &request);
        assert_eq!(binding.provider_id, provider, "{name}");
        assert_eq!(binding.settings.model, model, "{name}");
        assert_eq!(binding.settings.max_output_tokens, cap, "{name}");
        assert_eq!(
            binding.settings.base_url,
            format!("https://{provider}.test/v1"),
            "{name}"
        );
    }
}

fn row<'a>(label: &'a str, model: &'a str) -> RowDraft<'a> {
    RowDraft {
        label: Some(label),
        base_url: "http://127.0.0.1:11434/v1",
        model,
        auth_kind: AuthKind::ApiKey,
        max_output_tokens: None,
    }
}

/// A document from before PLAN 7.19 loads as the default row, and the next
/// save writes the roster shape and nothing else.
#[test]
fn a_singleton_document_becomes_the_default_row() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join(SETTINGS_FILE);
    fs::write(
        &path,
        br#"{"version":1,"provider":{"auth_kind":"claude_cli","base_url":"","model":"claude-sonnet-4-6"}}"#,
    )
    .expect("write");

    let store = SettingsStore::load(dir.path());
    let rows = store.list();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, DEFAULT_PROVIDER_ID);
    assert_eq!(rows[0].settings.auth_kind, AuthKind::ClaudeCli);
    assert_eq!(rows[0].settings.model, "claude-sonnet-4-6");

    store.add(&row("Local", "llama3")).expect("added");

    let written: Value =
        serde_json::from_slice(&fs::read(&path).expect("the document")).expect("json");
    assert!(written.get("provider").is_none(), "{written}");
    assert_eq!(written["version"], 1);
    let providers = written["providers"].as_array().expect("a list");
    assert_eq!(providers.len(), 2);
    assert_eq!(providers[0]["id"], DEFAULT_PROVIDER_ID);
    assert_eq!(providers[0]["model"], "claude-sonnet-4-6");
    assert_eq!(providers[1]["label"], "Local");
}

/// The trap PLAN 7.19 names: a save of one row must not drop the others,
/// nor a nested object a later build owns.
#[test]
fn a_save_round_trips_every_row_and_every_unknown_key() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join(SETTINGS_FILE);
    fs::write(
        &path,
        br#"{"version":1,"providers":[{"id":"default","model":"m"}],"decision":{"model":"jev"}}"#,
    )
    .expect("write");

    let store = SettingsStore::load(dir.path());
    let local = store.add(&row("Local", "llama3")).expect("added");
    store
        .set("https://x.test/v1", "m2", AuthKind::ApiKey, None)
        .expect("saved");

    let written: Value =
        serde_json::from_slice(&fs::read(&path).expect("the document")).expect("json");
    assert_eq!(written["decision"]["model"], "jev", "{written}");

    let reopened = SettingsStore::load(dir.path());
    let ids: Vec<String> = reopened.list().into_iter().map(|entry| entry.id).collect();
    assert_eq!(ids, [DEFAULT_PROVIDER_ID.to_owned(), local.id.clone()]);
    assert_eq!(reopened.get().model, "m2");
    assert_eq!(
        reopened.entry(&local.id).expect("kept").settings.model,
        "llama3"
    );
}

#[test]
fn a_roster_without_a_default_row_gets_one_first() {
    let dir = TempDir::new().expect("temp dir");
    fs::write(
        dir.path().join(SETTINGS_FILE),
        br#"{"version":1,"providers":[{"id":"x","model":"a"},{"id":"x","model":"b"},{"id":"default","model":"d"}]}"#,
    )
    .expect("write");

    let rows = SettingsStore::load(dir.path()).list();
    let ids: Vec<&str> = rows.iter().map(|entry| entry.id.as_str()).collect();
    assert_eq!(ids, [DEFAULT_PROVIDER_ID, "x"]);
    assert_eq!(rows[0].settings.model, "d");
    assert_eq!(rows[1].settings.model, "a", "the first of two repeats wins");
}

#[test]
fn an_added_row_gets_a_minted_id_and_a_checked_label() {
    let (_dir, store) = {
        let dir = TempDir::new().expect("temp dir");
        let store = SettingsStore::load(dir.path());
        (dir, store)
    };

    let added = store.add(&row("  Local  ", "llama3")).expect("added");
    assert!(Uuid::parse_str(&added.id).is_ok(), "{}", added.id);
    assert_eq!(added.label, "Local");
    assert!(store.contains(&added.id));

    let long = "x".repeat(LABEL_MAX_CHARS + 1);
    let err = store.add(&row(&long, "llama3")).expect_err("refused");
    assert_eq!(
        serde_json::to_value(&err).expect("serializes")["field"],
        "label"
    );
    assert_eq!(store.list().len(), 2, "a refusal adds nothing");
}

#[test]
fn an_update_keeps_the_label_unless_one_is_given() {
    let dir = TempDir::new().expect("temp dir");
    let store = SettingsStore::load(dir.path());
    let added = store.add(&row("Local", "llama3")).expect("added");

    let kept = store
        .update(
            &added.id,
            &RowDraft {
                label: None,
                ..row("", "qwen3")
            },
        )
        .expect("updated");
    assert_eq!(kept.label, "Local");
    assert_eq!(kept.settings.model, "qwen3");

    let err = store
        .update("no-such-row", &row("x", "m"))
        .expect_err("refused");
    assert!(matches!(err, AppError::ProviderNotFound { .. }), "{err}");
}

#[test]
fn the_roster_is_capped() {
    let dir = TempDir::new().expect("temp dir");
    let store = SettingsStore::load(dir.path());

    for _ in 1..PROVIDERS_MAX {
        store.add(&row("", "m")).expect("under the cap");
    }
    let err = store.add(&row("", "m")).expect_err("over the cap");
    assert!(err.to_string().contains("16"), "{err}");
    assert_eq!(store.list().len(), PROVIDERS_MAX);
}

#[test]
fn the_default_row_cannot_be_deleted_and_others_can() {
    let dir = TempDir::new().expect("temp dir");
    let store = SettingsStore::load(dir.path());
    let added = store.add(&row("Local", "llama3")).expect("added");

    let err = store.delete(DEFAULT_PROVIDER_ID).expect_err("refused");
    assert_eq!(
        serde_json::to_value(&err).expect("serializes")["code"],
        "E_INVALID_SETTING"
    );

    store.delete(&added.id).expect("deleted");
    assert!(!store.contains(&added.id));
    assert!(matches!(
        store.delete(&added.id),
        Err(AppError::ProviderNotFound { .. })
    ));
    assert_eq!(SettingsStore::load(dir.path()).list().len(), 1);
}

/// PLAN 7.18: a document from before the decision half loads with the
/// toggle on and both strings empty, resolved at use time.
#[test]
fn a_document_without_a_decision_key_loads_with_defaults() {
    let dir = TempDir::new().expect("temp dir");
    fs::write(
        dir.path().join(SETTINGS_FILE),
        br#"{"version":1,"providers":[{"id":"default","model":"m"}]}"#,
    )
    .expect("write");

    let decision = SettingsStore::load(dir.path()).decision();
    assert_eq!(decision, DecisionSettings::default());
    assert!(decision.annotate_approvals);
    assert_eq!(decision.resolved_model(), "jev-latest");
    assert_eq!(decision.resolved_base_url(), "https://api.typesafe.ai");
}

/// The trap: a decision save keeps every row, and a row save keeps the
/// decision settings.
#[test]
fn both_halves_survive_either_save() {
    let dir = TempDir::new().expect("temp dir");
    let store = SettingsStore::load(dir.path());
    let local = store.add(&row("Local", "llama3")).expect("added");

    store
        .set_decision("jev-2", "http://127.0.0.1:9/", false)
        .expect("saved");
    store
        .set("https://x.test/v1", "m2", AuthKind::ApiKey, None)
        .expect("saved");

    let reopened = SettingsStore::load(dir.path());
    assert!(reopened.contains(&local.id));
    assert_eq!(
        reopened.decision(),
        DecisionSettings {
            model: "jev-2".to_owned(),
            base_url: "http://127.0.0.1:9".to_owned(),
            annotate_approvals: false,
        }
    );
    let written = fs::read_to_string(store.path()).expect("the document");
    assert!(!written.contains("\"provider\""), "{written}");
}

#[test]
fn a_decision_url_that_names_the_endpoint_is_refused() {
    let dir = TempDir::new().expect("temp dir");
    let store = SettingsStore::load(dir.path());

    let err = store
        .set_decision("", "https://api.typesafe.ai/v1/systemone", true)
        .expect_err("refused");
    assert!(err.to_string().contains("appends that itself"), "{err}");
    assert!(normalize_decision_base_url("typesafe.ai")
        .expect_err("no scheme")
        .to_string()
        .contains("api.typesafe.ai"));
    assert_eq!(store.decision(), DecisionSettings::default());
}

/// PLAN 7.26: prices are set on their own, a row save keeps them, a bad list
/// changes nothing, and they survive a restart.
#[test]
fn prices_are_kept_through_a_row_save() {
    use crate::store::ledger::DOLLAR;

    let dir = TempDir::new().expect("temp dir");
    let store = SettingsStore::load(dir.path());
    let price = ModelPrice {
        model: "m".to_owned(),
        input: 3 * DOLLAR,
        output: 15 * DOLLAR,
        cache_read: None,
        cache_write: None,
    };

    store
        .set_prices(DEFAULT_PROVIDER_ID, std::slice::from_ref(&price))
        .expect("priced");
    store
        .update(DEFAULT_PROVIDER_ID, &row("Local", "m"))
        .expect("saved");
    assert_eq!(store.get().prices, std::slice::from_ref(&price));
    assert_eq!(store.get().price_of("m"), Some(&price));
    assert_eq!(store.get().price_of("other"), None);

    let twice = [price.clone(), price.clone()];
    assert!(store.set_prices(DEFAULT_PROVIDER_ID, &twice).is_err());
    assert_eq!(
        store.get().prices.len(),
        1,
        "a refused list changes nothing"
    );
    assert!(store.set_prices("nope", &[]).is_err());

    let reopened = SettingsStore::load(dir.path());
    assert_eq!(reopened.get().prices, [price]);
}
