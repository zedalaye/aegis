use super::*;

use serde_json::json;

/// One audit line, at defaults a test can override.
fn line(session: &str, turn: &str, tool: &str) -> AuditEntry {
    AuditEntry {
        ts: "2026-09-01T07:00:00.000Z".to_owned(),
        session_id: session.to_owned(),
        agent_id: "assistant".to_owned(),
        turn_id: turn.to_owned(),
        call_id: "call_1".to_owned(),
        tool: tool.to_owned(),
        skill: String::new(),
        handoff: String::new(),
        routine: String::new(),
        decision: AuditDecision::Auto,
        policy_reason: "an ordinary read inside the workspace".to_owned(),
        args_digest: "0".repeat(64),
        args_redacted: "{}".to_owned(),
        outcome: Outcome::Ok,
        duration_ms: 3,
        bytes_in: 0,
        bytes_out: 12,
        error_code: None,
        artifact: None,
    }
}

/// A session with one turn per id, each costing the same.
fn ledger(session: &str, turns: &[&str]) -> SessionLedger {
    SessionLedger {
        session_id: session.to_owned(),
        title: format!("{session} conversation"),
        routine: String::new(),
        handoff: String::new(),
        running: false,
        turns: turns
            .iter()
            .map(|turn| TurnCost::reported(turn, 100, 20))
            .collect(),
    }
}

fn find<'a>(runs: &'a [Run], kind: RunKind, id: &str) -> &'a Run {
    runs.iter()
        .find(|run| run.run.kind == kind && run.run.id == id)
        .unwrap_or_else(|| panic!("no {} run called {id}", kind.as_str()))
}

#[test]
fn the_widest_id_on_a_line_decides_which_run_it_is() {
    let mut entry = line("s1", "t1", "fs_read");
    entry.skill = "inbox.triage".to_owned();
    assert_eq!(RunRef::of(&entry).kind, RunKind::Skill);

    entry.routine = "r1".to_owned();
    assert_eq!(RunRef::of(&entry).kind, RunKind::Routine);

    entry.handoff = "h1".to_owned();
    let reference = RunRef::of(&entry);
    assert_eq!(reference.kind, RunKind::Handoff);
    assert!(
        reference.session_id.is_empty(),
        "a delegation is not a fact about one session"
    );
}

#[test]
fn a_delegation_is_one_run_over_the_cos_and_both_specialists() {
    let mut delegate = line("cos", "t1", "handoff_delegate");
    delegate.handoff = "h1".to_owned();
    let mut first = line("spec-a", "t9", "fs_write");
    first.handoff = "h1".to_owned();
    first.agent_id = "writer".to_owned();
    let mut second = line("spec-b", "t9", "fs_read");
    second.handoff = "h1".to_owned();
    second.agent_id = "reviewer".to_owned();

    let runs = fold(
        &[delegate, first, second],
        &[
            ledger("cos", &["t1"]),
            ledger("spec-a", &["t9"]),
            ledger("spec-b", &["t9"]),
        ],
    );

    let run = find(&runs, RunKind::Handoff, "h1");
    assert_eq!(run.calls, 3);
    assert_eq!(run.agents, ["assistant", "writer", "reviewer"]);
    assert_eq!(run.sessions, ["cos", "spec-a", "spec-b"]);
    // Three turns of 120 tokens each, in three different sessions: the join
    // is on the turn, not on the session.
    assert_eq!(run.cost.total(), 360);
}

#[test]
fn two_firings_of_one_routine_are_two_runs() {
    let mut monday = line("s1", "t1", "skill_run");
    monday.routine = "r1".to_owned();
    monday.ts = "2026-09-01T07:00:00.000Z".to_owned();
    let mut tuesday = line("s2", "t1", "skill_run");
    tuesday.routine = "r1".to_owned();
    tuesday.ts = "2026-09-02T07:00:00.000Z".to_owned();

    let runs = fold(
        &[monday, tuesday],
        &[ledger("s1", &["t1"]), ledger("s2", &["t1"])],
    );

    let firings: Vec<&Run> = runs
        .iter()
        .filter(|run| run.run.kind == RunKind::Routine)
        .collect();
    assert_eq!(firings.len(), 2, "a routine has many runs; each is one");
    assert_eq!(
        firings[0].run.session_id, "s2",
        "newest first, by what the run last did"
    );
}

#[test]
fn a_report_decides_the_status_even_when_something_was_refused() {
    let mut refused = line("s1", "t1", "fs_write");
    refused.skill = "inbox.triage".to_owned();
    refused.decision = AuditDecision::Deny;
    refused.outcome = Outcome::Denied;
    refused.policy_reason = "outside the workspace".to_owned();

    let mut report = line("s1", "t1", "skill_return");
    report.skill = "inbox.triage".to_owned();
    report.ts = "2026-09-01T07:00:01.000Z".to_owned();
    report.args_redacted = json!({
        "status": "done",
        "summary": "filed three tickets",
        "artefacts": [".aegis/artefacts/tickets.md"],
    })
    .to_string();

    let runs = fold(&[refused, report], &[ledger("s1", &["t1"])]);

    let run = find(&runs, RunKind::Skill, "inbox.triage");
    assert_eq!(run.status, RunStatus::Done);
    assert_eq!(run.denied, 1, "the refusal is still on the record");
    assert!(run.reason.is_empty());
    assert_eq!(run.artefacts, [".aegis/artefacts/tickets.md"]);
}

#[test]
fn a_run_that_only_failed_says_which_call_failed() {
    let mut refused = line("s1", "t1", "shell_exec");
    refused.skill = "watch.digest".to_owned();
    refused.decision = AuditDecision::Deny;
    refused.outcome = Outcome::Denied;
    refused.policy_reason = "the program is outside the workspace".to_owned();

    let runs = fold(&[refused], &[ledger("s1", &["t1"])]);

    let run = find(&runs, RunKind::Skill, "watch.digest");
    assert_eq!(run.status, RunStatus::Failed);
    assert!(
        run.reason.contains("shell_exec") && run.reason.contains("outside the workspace"),
        "why it failed, in the words the record used: {}",
        run.reason
    );
}

#[test]
fn a_runbook_that_never_reported_is_a_silence_and_a_silence_is_not_an_answer() {
    let mut opened = line("s1", "t1", "skill_run");
    opened.skill = "watch.digest".to_owned();

    let runs = fold(&[opened], &[ledger("s1", &["t1"])]);

    let run = find(&runs, RunKind::Skill, "watch.digest");
    assert_eq!(run.status, RunStatus::Failed);
    assert_eq!(run.reason, "it ended without reporting");
}

#[test]
fn a_conversation_cannot_fail_because_it_promised_nothing() {
    let mut refused = line("s1", "t1", "fs_write");
    refused.decision = AuditDecision::Deny;
    refused.outcome = Outcome::Denied;
    refused.policy_reason = "a write is a change to your files".to_owned();
    let mut broke = line("s1", "t1", "shell_exec");
    broke.outcome = Outcome::Error;
    broke.error_code = Some("E_TOOL_FAILED".to_owned());

    let runs = fold(&[refused, broke], &[ledger("s1", &["t1"])]);

    let run = find(&runs, RunKind::Session, "s1");
    assert_eq!(
        run.status,
        RunStatus::Ran,
        "a denial in a chat is the person's own answer, not a failed conversation"
    );
    assert!(run.reason.is_empty());
    assert_eq!(run.denied, 1, "what happened is still counted");
    assert_eq!(run.failed, 1);
}

#[test]
fn work_that_has_not_finished_has_not_failed() {
    let mut opened = line("s1", "t1", "skill_run");
    opened.skill = "watch.digest".to_owned();

    let mut session = ledger("s1", &["t1"]);
    session.running = true;

    let runs = fold(&[opened], &[session]);

    assert_eq!(
        find(&runs, RunKind::Skill, "watch.digest").status,
        RunStatus::Ran,
        "a runbook between its two calls looks exactly like one that stopped"
    );
}

#[test]
fn a_blocked_report_carries_the_question_it_left_open() {
    let mut report = line("s1", "t1", "handoff_return");
    report.handoff = "h1".to_owned();
    report.args_redacted = json!({
        "status": "blocked",
        "summary": "could not start",
        "open_questions": "which mailbox is the client one?",
    })
    .to_string();

    let runs = fold(&[report], &[ledger("s1", &["t1"])]);

    let run = find(&runs, RunKind::Handoff, "h1");
    assert_eq!(run.status, RunStatus::Blocked);
    assert_eq!(run.reason, "which mailbox is the client one?");
    assert!(run.status.is_stuck());
}

#[test]
fn needs_you_is_not_stuck_because_it_is_waiting_on_a_person() {
    assert!(!RunStatus::NeedsYou.is_stuck());
    assert!(RunStatus::Blocked.is_stuck());
    assert!(RunStatus::Failed.is_stuck());
    assert!(!RunStatus::Done.is_stuck());
    assert!(!RunStatus::Ran.is_stuck());
}

#[test]
fn the_runs_of_a_session_add_up_to_the_session() {
    // One conversation: a turn that ran a runbook, a turn that delegated,
    // a turn that called one tool, and a turn that only talked.
    let mut runbook = line("s1", "t1", "fs_read");
    runbook.skill = "inbox.triage".to_owned();
    let mut delegated = line("s1", "t2", "handoff_delegate");
    delegated.handoff = "h1".to_owned();
    let chatted = line("s1", "t3", "fs_list");

    let session = ledger("s1", &["t1", "t2", "t3", "t4"]);
    let whole = Cost::of(&session.turns);

    let runs = fold(&[runbook, delegated, chatted], &[session]);

    let summed = runs.iter().fold(Cost::default(), |mut total, run| {
        total.add(run.cost);
        total
    });
    assert_eq!(summed, whole, "every turn is charged to exactly one run");
    // `t4` called no tool at all and is still on the conversation's run,
    // beside the `fs_list` of `t3`.
    assert_eq!(find(&runs, RunKind::Session, "s1").cost.turns, 2);
}

#[test]
fn a_conversation_that_only_talked_is_still_a_run() {
    let runs = fold(&[], &[ledger("s1", &["t1", "t2"])]);

    let run = find(&runs, RunKind::Session, "s1");
    assert_eq!(run.calls, 0);
    assert_eq!(run.cost.turns, 2);
    assert_eq!(run.label, "s1 conversation");
    assert_eq!(run.sessions, ["s1"]);
}

#[test]
fn a_line_whose_session_is_not_this_projects_is_not_on_this_board() {
    let mine = line("s1", "t1", "fs_read");
    let theirs = line("s2", "t1", "fs_read");

    let runs = fold(&[mine, theirs], &[ledger("s1", &["t1"])]);

    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].run.id, "s1");
}

#[test]
fn artefacts_come_from_writes_captures_and_reports() {
    let mut wrote = line("s1", "t1", "fs_write");
    wrote.args_redacted =
        json!({ "path": ".aegis/artefacts/report.md", "content": "<40 bytes>" }).to_string();
    let mut captured = line("s1", "t1", "screen_capture");
    captured.artifact = Some(crate::audit::AuditArtifact {
        path: "captures/capture-1.png".to_owned(),
        sha256: "0".repeat(64),
        width: 100,
        height: 50,
    });
    let mut report = line("s1", "t1", "skill_return");
    report.args_redacted = json!({
        "status": "done",
        "artefacts": [".aegis/artefacts/report.md", ".aegis/artefacts/summary.md"],
    })
    .to_string();

    let runs = fold(&[wrote, captured, report], &[ledger("s1", &["t1"])]);

    let run = find(&runs, RunKind::Session, "s1");
    assert_eq!(
        run.artefacts,
        [
            ".aegis/artefacts/report.md",
            "captures/capture-1.png",
            ".aegis/artefacts/summary.md"
        ],
        "each path once, in the order it was produced"
    );
}

#[test]
fn a_refused_call_produced_nothing() {
    let mut refused = line("s1", "t1", "fs_write");
    refused.outcome = Outcome::Denied;
    refused.args_redacted = json!({ "path": ".aegis/artefacts/never.md" }).to_string();

    let runs = fold(&[refused], &[ledger("s1", &["t1"])]);

    assert!(
        find(&runs, RunKind::Session, "s1").artefacts.is_empty(),
        "a write that never ran left no file"
    );
}

#[test]
fn the_order_entries_arrive_in_does_not_change_the_fold() {
    let first = line("s1", "t1", "fs_read");
    let mut last = line("s1", "t1", "skill_return");
    last.ts = "2026-09-01T09:00:00.000Z".to_owned();
    last.args_redacted = json!({ "status": "done" }).to_string();

    let forwards = fold(&[first.clone(), last.clone()], &[ledger("s1", &["t1"])]);
    let backwards = fold(&[last, first], &[ledger("s1", &["t1"])]);

    assert_eq!(forwards, backwards);
    assert_eq!(forwards[0].started_at, "2026-09-01T07:00:00.000Z");
    assert_eq!(forwards[0].ended_at, "2026-09-01T09:00:00.000Z");
}

#[test]
fn a_routine_run_is_named_after_the_routine_not_its_id() {
    let mut fired = line("s1", "t1", "skill_run");
    fired.routine = "3f2c".to_owned();
    fired.skill = "watch.daily".to_owned();

    let mut session = ledger("s1", &["t1"]);
    session.routine = "Morning watch".to_owned();

    let runs = fold(&[fired], &[session]);

    let run = find(&runs, RunKind::Routine, "3f2c");
    assert_eq!(run.label, "Morning watch");
    assert_eq!(run.skill, "watch.daily", "the runbook it fired");
}

#[test]
fn a_turn_with_no_reported_usage_is_unknown_rather_than_free() {
    let session = SessionLedger {
        session_id: "s1".to_owned(),
        title: "Quiet".to_owned(),
        routine: String::new(),
        handoff: String::new(),
        running: false,
        turns: vec![TurnCost::reported("t1", 10, 5), TurnCost::unreported("t2")],
    };

    let runs = fold(&[line("s1", "t1", "fs_read")], &[session]);

    let run = find(&runs, RunKind::Session, "s1");
    assert_eq!(run.cost.total(), 15);
    assert_eq!(run.cost.turns, 2);
    assert_eq!(
        run.cost.unreported, 1,
        "at least this many tokens, not exactly this many"
    );
}
