//! Parking (PLAN 7.22): what the model is told, what the notification says,
//! and what the run records.

use std::sync::Mutex;

use tempfile::TempDir;

use super::*;
use crate::agent::Event;
use crate::policy::{ApprovalDetail, Grant, Risk};

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

/// A sink that keeps every event.
#[derive(Debug, Default)]
struct Seen {
    events: Mutex<Vec<Event>>,
}

impl EventSink for Seen {
    fn emit(&self, event: Event) {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(event);
    }
}

/// The ask a `shell_exec` outside what was signed raises. The summary is a
/// command line, which is exactly what must not reach a lock screen.
fn shell_ask() -> AskRequest {
    AskRequest {
        tool: "shell_exec".to_owned(),
        risk: Risk::High,
        title: "Run shell command",
        summary: "cargo test --secret-flag /ws/notes.md".to_owned(),
        detail: ApprovalDetail::Shell {
            program: "cargo".to_owned(),
            args: vec!["test".to_owned()],
            cwd: "/ws".to_owned(),
            shell_line: "cargo test".to_owned(),
            host: None,
        },
        grant: Some(Grant::shell("cargo")),
        scope_label: Grant::shell("cargo").scope_label(),
        reason: "this runs a program".to_owned(),
    }
}

/// Everything a park needs, on a temporary data directory.
struct Fixture {
    _dir: TempDir,
    store: ParkedStore,
    heard: Heard,
    seen: Seen,
    parks: Parks,
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new().expect("temp dir");
        let store = ParkedStore::load(dir.path());
        Self {
            _dir: dir,
            store,
            heard: Heard::default(),
            seen: Seen::default(),
            parks: Parks::new(),
        }
    }

    fn parking(&self) -> Parking<'_> {
        Parking {
            store: &self.store,
            notifier: &self.heard,
            project_id: "p1",
            routine_id: "r1",
            routine_name: "Morning watch",
            skill: "watch.digest",
            parks: &self.parks,
        }
    }

    fn call<'a>(&'a self, fingerprint: &'a str) -> Call<'a> {
        Call {
            session_id: "s1",
            agent_id: "a1",
            turn_id: "t1",
            call_id: "c1",
            fingerprint,
            cause: ParkCause::Unattended,
        }
    }
}

#[test]
fn a_parked_call_is_filed_announced_and_counted() {
    let fixture = Fixture::new();
    let ask = shell_ask();

    let outcome = park(
        &fixture.parking(),
        fixture.call("shell_exec:abc"),
        &ask,
        &fixture.seen,
    );

    let Outcome::Held(held) = outcome else {
        panic!("a run with a store parks");
    };
    assert_eq!(held.routine_name, "Morning watch");
    assert_eq!(held.skill, "watch.digest");
    assert_eq!(fixture.parks.ids(), vec![held.id.clone()]);
    assert!(fixture.parks.any());
    assert_eq!(fixture.store.list(None).len(), 1);
}

/// PLAN 7.22 refuses a notification that quotes an argument: the lock screen
/// gets the routine and a sentence, and the command line stays in the window.
#[test]
fn a_notification_names_the_routine_and_the_tool_and_nothing_else() {
    let fixture = Fixture::new();
    let ask = shell_ask();

    park(
        &fixture.parking(),
        fixture.call("shell_exec:abc"),
        &ask,
        &fixture.seen,
    );

    let notes = fixture.heard.notes();
    assert_eq!(notes.len(), 1, "one park, one notification");
    assert_eq!(notes[0].key, "r1", "coalesced per routine");
    assert_eq!(notes[0].title, "Morning watch");
    assert!(notes[0].body.contains("shell_exec"));
    assert!(
        !notes[0].body.contains("secret-flag") && !notes[0].body.contains("/ws/"),
        "{}: an argument must not reach a notification",
        notes[0].body
    );
}

/// The model is told to stop and say what it needed, not to find a way round.
#[test]
fn the_envelope_tells_the_model_to_return_blocked() {
    let fixture = Fixture::new();
    let ask = shell_ask();

    let Outcome::Held(held) = park(
        &fixture.parking(),
        fixture.call("shell_exec:abc"),
        &ask,
        &fixture.seen,
    ) else {
        panic!("it parks");
    };

    let envelope = envelope(&held);
    assert!(envelope.contains("parked"));
    assert!(envelope.contains("skill_return"));
    assert!(envelope.contains("blocked"));
    assert!(
        envelope.contains("resumed"),
        "{envelope}: a parked run is picked up again, and the model should know it"
    );
}

#[test]
fn a_run_may_not_park_for_ever() {
    let fixture = Fixture::new();
    let ask = shell_ask();

    for n in 0..MAX_PARKS_PER_RUN {
        let fingerprint = format!("shell_exec:{n}");
        let held = park(
            &fixture.parking(),
            Call {
                fingerprint: &fingerprint,
                ..fixture.call("unused")
            },
            &ask,
            &fixture.seen,
        );
        assert!(matches!(held, Outcome::Held(_)), "park {n}");
    }

    let over = park(
        &fixture.parking(),
        Call {
            fingerprint: "shell_exec:one-too-many",
            ..fixture.call("unused")
        },
        &ask,
        &fixture.seen,
    );
    let Outcome::Refused(reason) = over else {
        panic!("the budget is spent");
    };
    assert!(
        reason.contains("as many as one run may"),
        "{reason}: the refusal says why there is no question on the board"
    );
}

/// Without a store there is nowhere to park, and the wording is the one
/// Phase 16 used: no question reached anybody.
#[test]
fn the_unsigned_wording_says_what_was_not_signed() {
    let ask = shell_ask();
    let signable = unsigned(&ask);
    assert!(signable.contains("not signed for `shell_exec`"));
    assert!(signable.contains("blocked"));

    let ungrantable = unsigned(&AskRequest {
        grant: None,
        reason: "this file is outside the workspace".to_owned(),
        ..shell_ask()
    });
    assert!(
        ungrantable.contains("no routine can be signed for in advance"),
        "{ungrantable}"
    );
}

#[test]
fn a_resumption_says_which_answer_it_is() {
    let fixture = Fixture::new();
    let ask = shell_ask();
    let Outcome::Held(held) = park(
        &fixture.parking(),
        fixture.call("shell_exec:abc"),
        &ask,
        &fixture.seen,
    ) else {
        panic!("it parks");
    };

    let once = resumption(&held, Decision::AllowOnce);
    assert!(once.contains("allowed this once"));
    assert!(once.contains("unchanged"), "{once}");

    let standing = resumption(&held, Decision::AllowSession);
    assert!(standing.contains("standing approval"));

    let denied = resumption(&held, Decision::Deny);
    assert!(denied.contains("refused"));
    assert!(
        denied.contains("do not look for another way"),
        "{denied}: a refusal is not a puzzle"
    );

    assert_eq!(answer_word(Decision::AllowSession), "allow_standing");
}
