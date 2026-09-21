//! Model spend (PLAN 7.26): what a turn is refused before it sends, what a
//! round is charged, and who is told.

use std::sync::Mutex;

use tempfile::TempDir;

use super::*;
use crate::store::ledger::DOLLAR;
use crate::store::{Schedule, SpendCaps};

/// A notifier that keeps what it was given.
#[derive(Debug, Default)]
struct Heard {
    notes: Mutex<Vec<Note>>,
}

impl Heard {
    fn notes(&self) -> Vec<Note> {
        self.notes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl Notifier for Heard {
    fn post(&self, note: Note) {
        self.notes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(note);
    }
}

/// $1 in and $10 out per million tokens: 100 000 completion tokens are $1.
fn tariff(priced: bool) -> Tariff {
    Tariff {
        provider_id: "default".to_owned(),
        model: "m".to_owned(),
        price: priced.then(|| ModelPrice {
            model: "m".to_owned(),
            input: DOLLAR,
            output: 10 * DOLLAR,
            cache_read: None,
            cache_write: None,
        }),
        max_output_tokens: Some(50_000),
    }
}

fn agent(spend: SpendCaps) -> Agent {
    Agent {
        id: "a1".to_owned(),
        name: "Scribe".to_owned(),
        spend,
        ..Agent::stranded("a1")
    }
}

fn routine(spend: SpendCaps) -> Routine {
    Routine {
        id: "r1".to_owned(),
        name: "Morning watch".to_owned(),
        project_id: "p1".to_owned(),
        agent_id: "a1".to_owned(),
        skill: "watch.sweep".to_owned(),
        schedule: Schedule::Every { minutes: 60 },
        grants: Vec::new(),
        runs_per_day: 4,
        spend,
        runs_today: 0,
        paused: false,
        paused_reason: String::new(),
        armed_at: String::new(),
        last: None,
        problem: None,
        created_at: String::new(),
        updated_at: String::new(),
    }
}

/// Completion tokens worth `cents` at [`tariff`]'s price.
fn usage(cents: u64) -> Usage {
    Usage {
        prompt_tokens: 0,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        completion_tokens: cents * 1_000,
        total_tokens: cents * 1_000,
    }
}

const fn caps(per_run: Option<Micros>, per_day: Option<Micros>) -> SpendCaps {
    SpendCaps { per_run, per_day }
}

struct Fixture {
    _dir: TempDir,
    ledger: SpendLedger,
    heard: Heard,
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new().expect("temp dir");
        let ledger = SpendLedger::load(dir.path());
        Self {
            _dir: dir,
            ledger,
            heard: Heard::default(),
        }
    }

    fn meter<'a>(
        &'a self,
        priced: bool,
        agent: &'a Agent,
        routine: Option<&'a Routine>,
        session: &'a str,
        turn: &'a str,
    ) -> Meter<'a> {
        Meter::new(
            &self.ledger,
            &self.heard,
            tariff(priced),
            Payer {
                project_id: "p1",
                session_id: session,
                turn_id: turn,
                agent,
                routine,
                run_is_session: routine.is_some(),
            },
        )
    }
}

#[test]
fn an_uncapped_unpriced_turn_runs_and_records_nothing() {
    let fx = Fixture::new();
    let agent = agent(SpendCaps::default());
    let meter = fx.meter(false, &agent, None, "s1", "t1");
    assert_eq!(meter.refusal(), None);
    assert_eq!(meter.charge(Some(&usage(500)), 0), None);
    assert_eq!(fx.ledger.spent(Scope::Turn("t1")), 0);
}

#[test]
fn a_cap_on_an_unpriced_model_is_refused_before_the_first_request() {
    let fx = Fixture::new();
    let agent = agent(caps(Some(DOLLAR), None));
    let meter = fx.meter(false, &agent, None, "s1", "t1");
    let refusal = meter.refusal().expect("refused");
    assert!(refusal.contains("no price"), "{refusal}");
    assert!(refusal.contains("`m`"), "names the model: {refusal}");
}

#[test]
fn a_routine_run_halts_once_its_per_run_cap_is_passed() {
    let fx = Fixture::new();
    let agent = agent(SpendCaps::default());
    let routine = routine(caps(Some(DOLLAR / 2), None));
    let meter = fx.meter(true, &agent, Some(&routine), "s1", "t1");

    assert_eq!(meter.refusal(), None);
    assert_eq!(meter.charge(Some(&usage(30)), 0), None, "$0.30 of $0.50");
    let halted = meter.charge(Some(&usage(30)), 0).expect("past the cap");
    assert!(halted.contains("Morning watch"), "{halted}");
    assert!(halted.contains("per-run"), "{halted}");
    assert!(halted.contains("$0.50"), "{halted}");

    // A resume of the same run is the same session: it starts spent.
    let resumed = fx.meter(true, &agent, Some(&routine), "s1", "t2");
    assert!(resumed.refusal().is_some(), "a resume is the same run");

    // The next fire is a new session and starts clean.
    let next = fx.meter(true, &agent, Some(&routine), "s2", "t3");
    assert_eq!(next.refusal(), None);
}

#[test]
fn an_attended_run_is_one_turn() {
    let fx = Fixture::new();
    let agent = agent(caps(Some(DOLLAR / 10), None));
    let first = fx.meter(true, &agent, None, "s1", "t1");
    assert!(first.charge(Some(&usage(20)), 0).is_some());

    let second = fx.meter(true, &agent, None, "s1", "t2");
    assert_eq!(
        second.refusal(),
        None,
        "the next message in the same session is a new run"
    );
}

#[test]
fn a_day_cap_counts_every_turn_of_the_identity_and_notifies_twice() {
    let fx = Fixture::new();
    let agent = agent(caps(None, Some(DOLLAR)));

    let one = fx.meter(true, &agent, None, "s1", "t1");
    assert_eq!(one.charge(Some(&usage(50)), 0), None);
    assert!(fx.heard.notes().is_empty(), "50 % says nothing");

    let two = fx.meter(true, &agent, None, "s2", "t2");
    assert_eq!(two.charge(Some(&usage(35)), 0), None);
    let notes = fx.heard.notes();
    assert_eq!(notes.len(), 1, "85 % crosses 80 %");
    assert_eq!(notes[0].title, "Scribe");
    assert!(notes[0].body.contains("80 %"), "{}", notes[0].body);
    assert!(
        !notes[0].body.contains('$'),
        "never an amount on a lock screen"
    );

    assert!(two.charge(Some(&usage(20)), 0).is_some(), "past the day");
    let notes = fx.heard.notes();
    assert_eq!(notes.len(), 2);
    assert!(notes[1].body.contains("reached"), "{}", notes[1].body);

    // Any later turn today is refused before it sends.
    let three = fx.meter(true, &agent, None, "s3", "t3");
    let refused = three.refusal().expect("spent for today");
    assert!(refused.contains("daily"), "{refused}");
    assert_eq!(fx.heard.notes().len(), 2, "each crossing is told once");
}

#[test]
fn unreported_usage_is_estimated_high() {
    let fx = Fixture::new();
    let agent = agent(SpendCaps::default());
    let meter = fx.meter(true, &agent, None, "s1", "t1");
    // 30 000 prompt tokens at $1/M plus the row's 50 000-token ceiling at $10/M.
    meter.charge(None, 30_000);
    assert_eq!(fx.ledger.spent(Scope::Turn("t1")), 30_000 + 500_000);
}

#[test]
fn a_routines_caps_and_its_identitys_both_hold() {
    let fx = Fixture::new();
    let agent = agent(caps(None, Some(DOLLAR / 5)));
    let routine = routine(caps(Some(DOLLAR), None));
    let meter = fx.meter(true, &agent, Some(&routine), "s1", "t1");
    let halted = meter
        .charge(Some(&usage(25)), 0)
        .expect("the identity's day");
    assert!(halted.contains("identity `Scribe`"), "{halted}");
}

#[test]
fn an_estimate_counts_the_body_and_its_images() {
    use crate::agent::wire::WireImage;

    let text = "x".repeat(3_000);
    let plain = ModelRequest {
        model: "m".to_owned(),
        messages: vec![WireMessage::user(text.clone())],
        tools: Vec::new(),
    };
    let base = prompt_estimate(&plain);
    assert!(
        base >= 1_000,
        "3 000 bytes are at least 1 000 tokens: {base}"
    );

    let pictured = ModelRequest {
        messages: vec![WireMessage::User {
            content: text,
            images: vec![WireImage::File {
                path: "a.png".into(),
            }],
        }],
        ..plain
    };
    assert!(prompt_estimate(&pictured) >= base + IMAGE_TOKENS);
}
