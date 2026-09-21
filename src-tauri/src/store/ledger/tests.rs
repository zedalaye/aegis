use super::*;

use tempfile::TempDir;

fn price(input: Micros, output: Micros) -> ModelPrice {
    ModelPrice {
        model: "m".to_owned(),
        input,
        output,
        cache_read: None,
        cache_write: None,
    }
}

fn account<'a>(turn: &'a str, session: &'a str, routine: &'a str) -> Account<'a> {
    Account {
        turn_id: turn,
        session_id: session,
        project_id: "p",
        agent_id: "a",
        routine_id: routine,
        provider_id: "default",
        model: "m",
    }
}

#[test]
fn a_price_is_per_million_tokens_rounded_up() {
    // $3 in, $15 out per million.
    let price = price(3 * DOLLAR, 15 * DOLLAR);
    assert_eq!(price.cost(1_000_000, 0, 0, 0), 3 * DOLLAR);
    assert_eq!(price.cost(0, 0, 0, 1_000), 15_000);
    assert_eq!(price.cost(1, 0, 0, 0), 3, "3 micro-dollars, not zero");
    assert_eq!(
        price.cost(0, 0, 0, 1),
        15,
        "a single token is never free when it has a price"
    );
}

#[test]
fn cached_tokens_are_charged_at_their_own_price() {
    let price = ModelPrice {
        cache_read: Some(300_000),
        cache_write: Some(3_750_000),
        ..price(3 * DOLLAR, 15 * DOLLAR)
    };
    // 1M prompt, of which 800k read from the cache and 100k written to it.
    let cost = price.cost(1_000_000, 800_000, 100_000, 0);
    assert_eq!(cost, 300_000 + 240_000 + 375_000);
}

#[test]
fn a_blank_cache_price_charges_the_input_price() {
    let price = price(3 * DOLLAR, 0);
    assert_eq!(
        price.cost(1_000_000, 900_000, 0, 0),
        3 * DOLLAR,
        "errs high rather than guessing a discount"
    );
}

#[test]
fn dollars_read_as_money() {
    assert_eq!(dollars(500_000), "$0.50");
    assert_eq!(dollars(12 * DOLLAR), "$12.00");
    assert_eq!(dollars(1_234), "$0.001234");
    assert_eq!(dollars(0), "$0.00");
}

#[test]
fn caps_must_mean_something() {
    let zero = SpendCaps {
        per_run: Some(0),
        per_day: None,
    };
    assert!(zero.check().is_err());

    let inverted = SpendCaps {
        per_run: Some(2 * DOLLAR),
        per_day: Some(DOLLAR),
    };
    assert!(inverted.check().is_err());

    let huge = SpendCaps {
        per_run: None,
        per_day: Some(CAP_MAX + 1),
    };
    assert!(huge.check().is_err());

    let fine = SpendCaps {
        per_run: Some(DOLLAR / 2),
        per_day: Some(5 * DOLLAR),
    };
    assert_eq!(fine.check(), Ok(fine));
    assert!(!SpendCaps::default().any());
}

#[test]
fn prices_name_each_model_once() {
    let mut twice = vec![price(1, 1), price(2, 2)];
    assert!(check_prices(&twice).is_err());

    twice[1].model = "  other  ".to_owned();
    let checked = check_prices(&twice).expect("two models");
    assert_eq!(checked[1].model, "other", "trimmed");

    let blank = vec![ModelPrice {
        model: " ".to_owned(),
        ..price(1, 1)
    }];
    assert!(check_prices(&blank).is_err());

    let typo = vec![price(PRICE_MAX + 1, 1)];
    assert!(check_prices(&typo).is_err());
}

#[test]
fn charges_add_up_per_turn_session_and_day() {
    let dir = TempDir::new().expect("temp dir");
    let ledger = SpendLedger::load(dir.path());

    ledger
        .charge(&account("t1", "s1", "r1"), 100, false)
        .expect("charged");
    ledger
        .charge(&account("t1", "s1", "r1"), 50, true)
        .expect("charged");
    ledger
        .charge(&account("t2", "s1", "r1"), 10, false)
        .expect("charged");
    ledger
        .charge(&account("t3", "s2", ""), 1, false)
        .expect("charged");

    let day = today();
    assert_eq!(ledger.spent(Scope::Turn("t1")), 150);
    assert_eq!(ledger.spent(Scope::Session("s1")), 160);
    assert_eq!(ledger.spent(Scope::RoutineDay("r1", &day)), 160);
    assert_eq!(ledger.spent(Scope::AgentDay("a", &day)), 161);
    assert_eq!(ledger.spent(Scope::AgentDay("a", "2000-01-01")), 0);

    let today = ledger.today();
    assert_eq!(today.routines.get("r1"), Some(&160));
    assert_eq!(today.agents.get("a"), Some(&161));
    assert!(
        !today.routines.contains_key(""),
        "a turn with no routine is nobody's routine"
    );

    // Read back from disk: the rows survive a restart, estimated or not.
    let reloaded = SpendLedger::load(dir.path());
    assert_eq!(reloaded.spent(Scope::Session("s1")), 160);
    let rows = reloaded.rows();
    let first = rows.iter().find(|row| row.turn_id == "t1").expect("row");
    assert!(first.estimated, "one estimated round marks the turn");
}

#[test]
fn old_rows_are_pruned_and_bad_dates_are_kept() {
    let today = NaiveDate::from_ymd_opt(2026, 9, 21).expect("date");
    let row = |day: &str| SpendRow {
        turn_id: day.to_owned(),
        session_id: String::new(),
        project_id: String::new(),
        agent_id: String::new(),
        routine_id: String::new(),
        provider_id: String::new(),
        model: String::new(),
        day: day.to_owned(),
        at: String::new(),
        micros: 1,
        estimated: false,
    };
    let mut rows = vec![row("2026-01-01"), row("2026-09-01"), row("garbled")];
    prune(&mut rows, today);
    let kept: Vec<&str> = rows.iter().map(|row| row.day.as_str()).collect();
    assert_eq!(kept, ["2026-09-01", "garbled"]);
}

#[test]
fn a_damaged_ledger_is_moved_aside() {
    let dir = TempDir::new().expect("temp dir");
    fs::write(dir.path().join(LEDGER_FILE), b"{ not json").expect("write");
    let ledger = SpendLedger::load(dir.path());
    assert_eq!(ledger.spent(Scope::Turn("t")), 0);
    let moved = fs::read_dir(dir.path())
        .expect("list")
        .filter_map(Result::ok)
        .any(|entry| entry.file_name().to_string_lossy().contains("corrupt"));
    assert!(moved);
}
