//! The parked document (PLAN 7.22): what it keeps, what it refuses to keep
//! twice, and what it lets go of.

use chrono::TimeDelta;
use tempfile::TempDir;

use super::*;
use crate::policy::{ApprovalDetail, Grant, Risk};

/// A store on a temporary data directory.
fn store() -> (TempDir, ParkedStore) {
    let dir = TempDir::new().expect("temp dir");
    let store = ParkedStore::load(dir.path());
    (dir, store)
}

/// The ask a `fs_write` inside the workspace raises.
fn write_ask() -> crate::policy::AskRequest {
    crate::policy::AskRequest {
        tool: "fs_write".to_owned(),
        risk: Risk::Medium,
        title: "Write file",
        summary: "notes.md (12 B, new file)".to_owned(),
        detail: ApprovalDetail::FsWrite {
            path: "/ws/notes.md".to_owned(),
            bytes: 12,
            exists: false,
            preview: Some("hello".to_owned()),
            applies: None,
        },
        grant: Some(Grant::FsWrite),
        scope_label: Grant::FsWrite.scope_label(),
        reason: "this creates a file in the workspace".to_owned(),
    }
}

/// A draft for one run's call.
fn draft<'a>(
    session_id: &'a str,
    fingerprint: &str,
    request: &'a crate::policy::AskRequest,
) -> ParkDraft<'a> {
    ParkDraft {
        project_id: "p1",
        session_id,
        agent_id: "a1",
        routine_id: "r1",
        routine_name: "Morning watch",
        skill: "watch.digest",
        turn_id: "t1",
        call_id: "c1",
        fingerprint: fingerprint.to_owned(),
        cause: ParkCause::Unattended,
        request,
    }
}

#[test]
fn a_park_keeps_everything_the_dialog_would_have_shown() {
    let (_dir, store) = store();
    let ask = write_ask();

    let parked = store
        .park(&draft("s1", "fs_write:abc", &ask))
        .expect("the first park is recorded");

    assert_eq!(parked.tool, "fs_write");
    assert_eq!(parked.summary, ask.summary);
    assert_eq!(parked.detail, ask.detail);
    assert_eq!(parked.grant, Some(Grant::FsWrite));
    assert_eq!(
        parked.scope_label,
        Grant::FsWrite.standing_label(),
        "signing this onto a routine outlives any one run of it"
    );
    assert_eq!(parked.risk, Risk::Medium);
    assert!(
        parked.expires_at > parked.parked_at,
        "{} is not after {}",
        parked.expires_at,
        parked.parked_at
    );
}

/// A run that repeats a call it has already parked is asking one question, not
/// two — and so is a clock that fires again and stops at the same place.
#[test]
fn the_same_call_parked_twice_is_one_question() {
    let (_dir, store) = store();
    let ask = write_ask();

    let first = store
        .park(&draft("s1", "fs_write:abc", &ask))
        .expect("first");
    let again = store
        .park(&draft("s1", "fs_write:abc", &ask))
        .expect("the same call");
    assert_eq!(first.id, again.id);

    // The next run of the same routine, in its own session.
    let tomorrow = store
        .park(&draft("s2", "fs_write:abc", &ask))
        .expect("the next run");
    assert_eq!(
        tomorrow.id, first.id,
        "answering it once answers it, rather than one row per fire"
    );
    assert_eq!(
        tomorrow.session_id, "s1",
        "the run an answer resumes is the one holding the work"
    );
    assert_eq!(store.list(None).len(), 1);

    // A different call is a different question.
    store
        .park(&draft("s2", "fs_write:def", &ask))
        .expect("another call");
    assert_eq!(store.list(None).len(), 2);
}

#[test]
fn one_run_may_park_three_calls_and_no_more() {
    let (_dir, store) = store();
    let ask = write_ask();

    for n in 0..MAX_PARKS_PER_RUN {
        store
            .park(&draft("s1", &format!("fs_write:{n}"), &ask))
            .expect("inside the budget");
    }

    let err = store
        .park(&draft("s1", "fs_write:one-too-many", &ask))
        .expect_err("the budget is spent");
    assert!(matches!(err, AppError::ParkBudget { .. }), "{err}");

    // Another run's budget is its own.
    store
        .park(&draft("s2", "fs_write:abc", &ask))
        .expect("a different run");
    assert_eq!(store.list(None).len(), MAX_PARKS_PER_RUN + 1);
}

#[test]
fn a_park_is_taken_once_and_survives_a_reload() {
    let dir = TempDir::new().expect("temp dir");
    let ask = write_ask();

    let id = {
        let store = ParkedStore::load(dir.path());
        store
            .park(&draft("s1", "fs_write:abc", &ask))
            .expect("parked")
            .id
    };

    // A second process, reading the document off disk.
    let store = ParkedStore::load(dir.path());
    assert_eq!(store.list(Some("p1")).len(), 1);
    assert_eq!(store.list(Some("elsewhere")), Vec::new());
    assert_eq!(store.get(&id).expect("still open").id, id);

    assert!(store.take(&id).is_some());
    assert!(store.take(&id).is_none(), "an answer is spent once");
    assert!(store.get(&id).is_err());
}

/// PLAN 7.22: seven days unanswered, and the question is stale.
#[test]
fn a_park_nobody_answers_expires() {
    let (_dir, store) = store();
    let ask = write_ask();
    let parked = store
        .park(&draft("s1", "fs_write:abc", &ask))
        .expect("parked");

    assert!(
        store.prune(Utc::now()).is_empty(),
        "a fresh park is not stale"
    );

    let later = Utc::now() + TimeDelta::days(PARK_TTL_DAYS + 1);
    let expired = store.prune(later);
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].id, parked.id);
    assert!(store.list(None).is_empty());
}

#[test]
fn a_closed_session_and_a_forgotten_project_take_their_parks_with_them() {
    let (_dir, store) = store();
    let ask = write_ask();
    store.park(&draft("s1", "fs_write:abc", &ask)).expect("one");
    store.park(&draft("s2", "fs_write:def", &ask)).expect("two");

    assert!(store.forget_session("s1"));
    assert!(!store.forget_session("s1"), "forgetting is idempotent");
    assert_eq!(store.list(None).len(), 1);

    assert!(store.forget_project("p1"));
    assert!(store.list(None).is_empty());
}

/// A damaged document must not stop the application (`docs/guide/data.md`).
#[test]
fn an_unreadable_document_starts_empty_rather_than_failing() {
    let dir = TempDir::new().expect("temp dir");
    std::fs::write(dir.path().join(PARKED_FILE), b"{not json").expect("write");

    let store = ParkedStore::load(dir.path());
    assert!(store.list(None).is_empty());
    assert!(
        store.path().with_extension("json").exists() || store.list(None).is_empty(),
        "the damaged file was moved aside"
    );
}
