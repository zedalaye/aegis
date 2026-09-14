//! The status board (PLAN 7.3, Phase 17; PLAN 7.2, rows 3 and 9).
//!
//! The structured read of `/status`, merged with what only the runtime knows.
//!
//! ```text
//!  .aegis/status/STATUS.md ─┐
//!                    ├─▶ Attention ── In flight ── Blocked
//!  approvals ────────┤
//!  sessions ─────────┤
//!  routines ─────────┤
//!  runs (trace) ─────┘
//! ```
//!
//! The **file** holds what somebody decided is true; the **runtime** half is
//! what this process knows now. Each line says which it came from. Nothing here
//! writes the file — corrections are `fs_write` under the gate (PLAN 7.6).
//!
//! * **Attention**: a person must act (approval, `needs_you`, a routine that
//!   gave up).
//! * **In flight**: running now.
//! * **Blocked**: stopped short, not waiting on a person.
//!
//! All-emphasis placeholder lines (`_Nothing blocked._`) are dropped by
//! [`sections`], so an empty column stays empty (PLAN 7.2 row 9).

pub mod trace;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::approval::ApprovalRequest;
use crate::store::{Cost, Routine, SessionState, SessionSummary};

/// Most items one column will hold: a board fits one screen.
const COLUMN_MAX: usize = 40;

/// Most stuck runs the *Blocked* column names; the run list has the rest.
const STUCK_MAX: usize = 8;

/// Longest line kept from the file.
const LINE_MAX_CHARS: usize = 200;

// ---------------------------------------------------------------------------
// The payload
// ---------------------------------------------------------------------------

/// Where a line on the board came from.
///
/// On every item, since the file and the runtime are not equally current.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts", rename = "BoardSource")]
pub enum Source {
    /// A line of `.aegis/status/STATUS.md`.
    Status,
    /// An approval dialog waiting to be answered.
    Approval,
    /// A session, as the turn registry sees it.
    Session,
    /// A routine, as the scheduler sees it.
    Routine,
    /// A run, folded from the audit log.
    Run,
}

/// One line of the board.
///
/// `BoardItem` on the wire: the generated bindings share one namespace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings.ts", rename = "BoardItem")]
pub struct Item {
    /// The line itself.
    pub text: String,
    /// What is worth knowing beside it: a reason, a runbook, an identity.
    /// Empty when the line says it all.
    pub detail: String,
    /// Where it came from.
    pub source: Source,
    /// RFC3339 UTC, when the runtime knows. Empty for a line of the file,
    /// which carries no time of its own.
    pub at: String,
    /// The session to open. Empty when there is none to open.
    pub session_id: String,
    /// The routine this is about. Empty when it is not about one.
    pub routine_id: String,
    /// The run to trace, when this line has one.
    pub run: Option<trace::RunRef>,
}

impl Item {
    /// A line with nothing to click.
    fn plain(source: Source, text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            detail: String::new(),
            source,
            at: String::new(),
            session_id: String::new(),
            routine_id: String::new(),
            run: None,
        }
    }

    /// Adds the sentence under the line.
    fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    /// Points the line at a session.
    fn session(mut self, session_id: &str) -> Self {
        self.session_id = session_id.to_owned();
        self
    }

    /// Stamps the line.
    fn at(mut self, at: &str) -> Self {
        self.at = at.to_owned();
        self
    }
}

/// The board of one project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct Board {
    /// The project it is the board of.
    pub project_id: String,
    /// Where `STATUS.md` is, when it is there. Empty when the convention has
    /// not been laid down in this workspace, which is a thing the panel says
    /// rather than an error.
    pub status_path: String,
    /// A person has to do something.
    pub attention: Vec<Item>,
    /// Something is running.
    pub in_flight: Vec<Item>,
    /// Something stopped short.
    pub blocked: Vec<Item>,
    /// Every run in the window, newest first.
    pub runs: Vec<trace::Run>,
    /// What every session of the project has spent — not just the runs in the
    /// audit window.
    pub cost: Cost,
}

/// The three columns, as the file spells them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sections {
    /// Under *Attention*.
    pub attention: Vec<String>,
    /// Under *In flight*.
    pub in_flight: Vec<String>,
    /// Under *Blocked*.
    pub blocked: Vec<String>,
}

/// Everything a board is assembled from, besides the file.
#[derive(Debug)]
pub struct Facts<'a> {
    /// The project.
    pub project_id: &'a str,
    /// Where `.aegis/status/STATUS.md` is, and what it says. `None` when there is no
    /// such file.
    pub status: Option<(&'a str, &'a str)>,
    /// The project's sessions, with their live state already measured.
    pub sessions: &'a [SessionSummary],
    /// The project's routines, with their problems already derived.
    pub routines: &'a [Routine],
    /// Approvals waiting to be answered, in this project's sessions.
    pub approvals: &'a [ApprovalRequest],
    /// The runs folded out of the audit log, newest first.
    pub runs: Vec<trace::Run>,
}

// ---------------------------------------------------------------------------
// The structured read of the file
// ---------------------------------------------------------------------------

/// The headings each column answers to, lower-cased.
///
/// Several spellings per column; an unrecognized heading ends the section.
const HEADINGS: [(&str, &[&str]); 3] = [
    ("attention", &["attention", "needs you", "needs a human"]),
    ("in flight", &["in flight", "in-flight", "running"]),
    ("blocked", &["blocked", "stuck"]),
];

/// Reads `STATUS.md` into its three columns.
///
/// One line, one item; unrecognized lines are kept verbatim. Dropped: blank
/// lines, fenced or indented blocks, all-emphasis lines, and anything before
/// the first recognized heading.
pub fn sections(text: &str) -> Sections {
    let mut found = Sections::default();
    let mut column: Option<usize> = None;

    for line in text.lines() {
        let trimmed = line.trim();

        if let Some(heading) = trimmed.strip_prefix('#') {
            let name = heading.trim_start_matches('#').trim().to_lowercase();
            column = HEADINGS
                .iter()
                .position(|(_, spellings)| spellings.contains(&name.as_str()));
            continue;
        }

        let Some(index) = column else { continue };
        // An indented block is an example, not an item: the seeded `.aegis/briefs/`
        // and `decisions/` files both use one, and a board that copied its own
        // template into itself would be unreadable.
        if line.starts_with("    ") || line.starts_with('\t') {
            continue;
        }
        let Some(item) = item(trimmed) else { continue };

        let column = match index {
            0 => &mut found.attention,
            1 => &mut found.in_flight,
            _ => &mut found.blocked,
        };
        if column.len() < COLUMN_MAX {
            column.push(item);
        }
    }

    found
}

/// One line of the file as a board item, or nothing.
fn item(trimmed: &str) -> Option<String> {
    if trimmed.is_empty() || trimmed.starts_with("```") || trimmed.starts_with("<!--") {
        return None;
    }

    // A bullet or a numbered item is a line with a marker in front of it.
    let text = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))
        .unwrap_or(trimmed)
        .trim();
    if text.is_empty() {
        return None;
    }

    if is_placeholder(text) {
        return None;
    }

    let mut kept: String = text.chars().take(LINE_MAX_CHARS).collect();
    if kept.chars().count() < text.chars().count() {
        kept.push('…');
    }
    Some(kept)
}

/// Whether a line is a whole line of emphasis — the seed's way of saying
/// nothing is here.
fn is_placeholder(text: &str) -> bool {
    for mark in ["**", "__", "*", "_"] {
        if let Some(inner) = text
            .strip_prefix(mark)
            .and_then(|rest| rest.strip_suffix(mark))
        {
            if !inner.is_empty() && !inner.contains(mark) {
                return true;
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// The assembly
// ---------------------------------------------------------------------------

/// Builds the board.
///
/// Runtime lines first in each column (certainly current), then the file's.
pub fn assemble(facts: Facts<'_>) -> Board {
    let Facts {
        project_id,
        status,
        sessions,
        routines,
        approvals,
        runs,
    } = facts;

    let mut board = Board {
        project_id: project_id.to_owned(),
        status_path: status.map(|(path, _)| path.to_owned()).unwrap_or_default(),
        attention: Vec::new(),
        in_flight: Vec::new(),
        blocked: Vec::new(),
        cost: sessions.iter().fold(Cost::default(), |mut total, session| {
            total.add(session.cost);
            total
        }),
        runs,
    };

    // --- Attention: somebody has to do something -------------------------

    for request in approvals {
        board.attention.push(
            Item::plain(
                Source::Approval,
                format!("{} is waiting for you", request.title),
            )
            .detail(request.summary.clone())
            .session(&request.session_id)
            .at(&request.requested_at),
        );
    }

    // A routine the *scheduler* stopped, which `COS.md` reaches only after two
    // silences: escalate to the human. A routine a person paused is not here,
    // because nobody is waiting on anybody.
    for routine in routines {
        if routine.paused && !routine.paused_reason.is_empty() {
            let mut item = Item::plain(Source::Routine, format!("{} stopped itself", routine.name))
                .detail(routine.paused_reason.clone());
            item.routine_id = routine.id.clone();
            board.attention.push(item);
        }
    }

    for run in board
        .runs
        .iter()
        .filter(|run| run.status == trace::RunStatus::NeedsYou)
        .take(STUCK_MAX)
    {
        board.attention.push(from_run(run));
    }

    // --- In flight: something is running now ------------------------------

    for session in sessions {
        let word = match session.state {
            SessionState::Running => "running",
            // A turn parked on a dialog also appears in Attention, via its
            // approval.
            SessionState::AwaitingApproval => "waiting on an approval",
            SessionState::Idle | SessionState::Error => continue,
        };
        board.in_flight.push(
            Item::plain(Source::Session, session.title.clone())
                .detail(match &session.scheduled {
                    Some(scheduled) => format!("{word} — {}", scheduled.routine_name),
                    None => word.to_owned(),
                })
                .session(&session.id)
                .at(&session.updated_at),
        );
    }

    // --- Blocked: something stopped short ---------------------------------

    // Routines that cannot fire now (`schedule::inspect`).
    for routine in routines {
        let Some(problem) = &routine.problem else {
            continue;
        };
        let mut item = Item::plain(Source::Routine, format!("{} cannot fire", routine.name))
            .detail(problem.clone());
        item.routine_id = routine.id.clone();
        board.blocked.push(item);
    }

    for run in board
        .runs
        .iter()
        .filter(|run| run.status.is_stuck())
        .take(STUCK_MAX)
    {
        board.blocked.push(from_run(run));
    }

    // --- Then the file ----------------------------------------------------

    if let Some((_, text)) = status {
        let sections = sections(text);
        board.attention.extend(
            sections
                .attention
                .into_iter()
                .map(|line| Item::plain(Source::Status, line)),
        );
        board.in_flight.extend(
            sections
                .in_flight
                .into_iter()
                .map(|line| Item::plain(Source::Status, line)),
        );
        board.blocked.extend(
            sections
                .blocked
                .into_iter()
                .map(|line| Item::plain(Source::Status, line)),
        );
    }

    board.attention.truncate(COLUMN_MAX);
    board.in_flight.truncate(COLUMN_MAX);
    board.blocked.truncate(COLUMN_MAX);
    board
}

/// A run as a line on the board.
fn from_run(run: &trace::Run) -> Item {
    let mut item = Item::plain(Source::Run, run.label.clone())
        .detail(if run.reason.is_empty() {
            format!("{} — {} calls", run.status.as_str(), run.calls)
        } else {
            run.reason.clone()
        })
        .at(&run.ended_at);
    item.session_id = run.sessions.first().cloned().unwrap_or_default();
    item.run = Some(run.run.clone());
    item
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::policy::{ApprovalDetail, Risk};
    use crate::store::{Schedule, Scheduled};

    /// The seed `scaffold` writes into a fresh workspace.
    const SEEDED: &str = "\
# Status

What is true right now. Rewritten in place, not appended to — this is a board,
not a log.

## Attention

_Nothing waiting on a human._

## In flight

_Nothing running._

## Blocked

_Nothing blocked._
";

    fn session(id: &str, state: SessionState) -> SessionSummary {
        SessionSummary {
            id: id.to_owned(),
            project_id: "p1".to_owned(),
            agent_id: "assistant".to_owned(),
            title: format!("{id} conversation"),
            created_at: "2026-09-01T07:00:00.000Z".to_owned(),
            updated_at: "2026-09-01T07:30:00.000Z".to_owned(),
            message_count: 4,
            state,
            delegated: None,
            scheduled: None,
            cost: Cost::default(),
        }
    }

    fn routine(name: &str) -> Routine {
        Routine {
            id: format!("id-{name}"),
            name: name.to_owned(),
            project_id: "p1".to_owned(),
            agent_id: "assistant".to_owned(),
            skill: "watch.daily".to_owned(),
            schedule: Schedule::Every { minutes: 60 },
            grants: Vec::new(),
            runs_per_day: 4,
            runs_today: 1,
            paused: false,
            paused_reason: String::new(),
            armed_at: "2026-09-01T06:00:00.000Z".to_owned(),
            last: None,
            problem: None,
            created_at: "2026-09-01T06:00:00.000Z".to_owned(),
            updated_at: "2026-09-01T06:00:00.000Z".to_owned(),
        }
    }

    fn approval(session_id: &str) -> ApprovalRequest {
        ApprovalRequest {
            request_id: "req-1".to_owned(),
            session_id: session_id.to_owned(),
            turn_id: "t1".to_owned(),
            call_id: "call_1".to_owned(),
            tool: "fs_write".to_owned(),
            risk: Risk::Medium,
            title: "Write file".to_owned(),
            summary: ".aegis/decisions/DECISIONS.md".to_owned(),
            detail: ApprovalDetail::FsWrite {
                path: ".aegis/decisions/DECISIONS.md".to_owned(),
                bytes: 42,
                exists: true,
                preview: None,
                applies: None,
            },
            scope_label: "writes inside this workspace".to_owned(),
            session_grant_allowed: true,
            reason: "a write is a change to your files".to_owned(),
            requested_at: "2026-09-01T07:31:00.000Z".to_owned(),
            expires_at: "2026-09-01T07:36:00.000Z".to_owned(),
        }
    }

    fn facts<'a>(
        status: Option<&'a str>,
        sessions: &'a [SessionSummary],
        routines: &'a [Routine],
        approvals: &'a [ApprovalRequest],
        runs: Vec<trace::Run>,
    ) -> Facts<'a> {
        Facts {
            project_id: "p1",
            status: status.map(|text| ("/w/status/STATUS.md", text)),
            sessions,
            routines,
            approvals,
            runs,
        }
    }

    fn run(label: &str, status: trace::RunStatus, reason: &str) -> trace::Run {
        let mut folded = trace::fold(
            &[],
            &[trace::SessionLedger {
                session_id: "s1".to_owned(),
                title: label.to_owned(),
                routine: String::new(),
                handoff: String::new(),
                running: false,
                turns: vec![crate::store::TurnCost::reported("t1", 10, 5)],
            }],
        );
        let mut run = folded.pop().expect("one run");
        run.status = status;
        run.reason = reason.to_owned();
        run
    }

    // --- the structured read ------------------------------------------------

    #[test]
    fn a_freshly_seeded_board_says_nothing_at_all() {
        let read = sections(SEEDED);

        assert!(read.attention.is_empty(), "{:?}", read.attention);
        assert!(read.in_flight.is_empty());
        assert!(read.blocked.is_empty());
    }

    #[test]
    fn bullets_and_prose_are_items_and_markers_are_not_kept() {
        let read = sections(
            "## Attention\n- Call the client back\n* Sign the quote\nThe invoice is late\n\
             \n## Blocked\n1. waiting on VAT number\n",
        );

        assert_eq!(
            read.attention,
            [
                "Call the client back",
                "Sign the quote",
                "The invoice is late"
            ]
        );
        assert_eq!(
            read.blocked,
            ["1. waiting on VAT number"],
            "a numbered marker is part of the line rather than a bullet to strip"
        );
    }

    #[test]
    fn an_unrecognized_heading_ends_the_section_it_follows() {
        let read = sections("## Attention\n- one\n\n## Notes\n- not a board item\n");

        assert_eq!(read.attention, ["one"]);
        assert!(read.in_flight.is_empty());
        assert!(read.blocked.is_empty());
    }

    #[test]
    fn everything_before_the_first_heading_is_prose_about_the_file() {
        let read =
            sections("# Status\n\nWhat is true right now.\n\n## Blocked\n- the VAT number\n");

        assert_eq!(read.blocked, ["the VAT number"]);
        assert!(read.attention.is_empty());
    }

    #[test]
    fn an_example_block_is_not_a_board_item() {
        let read = sections("## In flight\n\n    goal:\n    owner:\n\n- the actual work\n");

        assert_eq!(read.in_flight, ["the actual work"]);
    }

    #[test]
    fn the_headings_are_read_however_they_are_spelled() {
        let read = sections("### needs you\n- one\n### In-flight\n- two\n### Stuck\n- three\n");

        assert_eq!(read.attention, ["one"]);
        assert_eq!(read.in_flight, ["two"]);
        assert_eq!(read.blocked, ["three"]);
    }

    // --- the assembly -------------------------------------------------------

    #[test]
    fn a_project_with_nothing_happening_has_an_empty_board() {
        let board = assemble(facts(Some(SEEDED), &[], &[], &[], Vec::new()));

        assert!(board.attention.is_empty());
        assert!(board.in_flight.is_empty());
        assert!(board.blocked.is_empty());
        assert_eq!(board.status_path, "/w/status/STATUS.md");
    }

    #[test]
    fn a_workspace_with_no_status_file_still_has_a_board() {
        let sessions = [session("s1", SessionState::Running)];
        let board = assemble(facts(None, &sessions, &[], &[], Vec::new()));

        assert!(
            board.status_path.is_empty(),
            "the panel says the convention is not laid down; it is not an error"
        );
        assert_eq!(board.in_flight.len(), 1);
    }

    #[test]
    fn an_approval_is_attention_and_the_turn_it_parked_is_in_flight() {
        let sessions = [session("s1", SessionState::AwaitingApproval)];
        let approvals = [approval("s1")];
        let board = assemble(facts(Some(SEEDED), &sessions, &[], &approvals, Vec::new()));

        assert_eq!(board.attention.len(), 1);
        assert_eq!(board.attention[0].source, Source::Approval);
        assert_eq!(board.attention[0].session_id, "s1");
        assert!(board.attention[0].text.contains("Write file"));

        assert_eq!(board.in_flight.len(), 1);
        assert_eq!(board.in_flight[0].detail, "waiting on an approval");
    }

    #[test]
    fn a_routine_that_stopped_itself_asks_for_a_person() {
        let mut stopped = routine("Morning watch");
        stopped.paused = true;
        stopped.paused_reason = "two runs in a row returned nothing".to_owned();
        let mut by_hand = routine("Evening watch");
        by_hand.paused = true;

        let routines = [stopped, by_hand];
        let board = assemble(facts(Some(SEEDED), &[], &routines, &[], Vec::new()));

        assert_eq!(
            board.attention.len(),
            1,
            "a routine a person paused is not a question"
        );
        assert_eq!(board.attention[0].text, "Morning watch stopped itself");
        assert_eq!(board.attention[0].routine_id, "id-Morning watch");
    }

    #[test]
    fn a_routine_that_cannot_fire_is_blocked_rather_than_waiting() {
        let mut broken = routine("Morning watch");
        broken.problem = Some("that identity is no longer granted watch.daily".to_owned());

        let routines = [broken];
        let board = assemble(facts(Some(SEEDED), &[], &routines, &[], Vec::new()));

        assert!(board.attention.is_empty());
        assert_eq!(board.blocked.len(), 1);
        assert_eq!(board.blocked[0].text, "Morning watch cannot fire");
        assert_eq!(
            board.blocked[0].detail,
            "that identity is no longer granted watch.daily"
        );
    }

    #[test]
    fn who_has_to_move_next_decides_which_column_a_run_lands_in() {
        let runs = vec![
            run(
                "Ask the client",
                trace::RunStatus::NeedsYou,
                "which mailbox?",
            ),
            run(
                "Triage the inbox",
                trace::RunStatus::Blocked,
                "no such folder",
            ),
            run("Draft the report", trace::RunStatus::Done, ""),
        ];
        let board = assemble(facts(Some(SEEDED), &[], &[], &[], runs));

        assert_eq!(board.attention.len(), 1);
        assert_eq!(board.attention[0].text, "Ask the client");
        assert_eq!(board.attention[0].detail, "which mailbox?");
        assert_eq!(board.blocked.len(), 1);
        assert_eq!(board.blocked[0].text, "Triage the inbox");
    }

    #[test]
    fn the_runtime_speaks_first_and_the_file_follows() {
        let sessions = [session("s1", SessionState::Running)];
        let board = assemble(facts(
            Some("## In flight\n- the quarterly review\n"),
            &sessions,
            &[],
            &[],
            Vec::new(),
        ));

        assert_eq!(board.in_flight.len(), 2);
        assert_eq!(board.in_flight[0].source, Source::Session);
        assert_eq!(board.in_flight[1].source, Source::Status);
        assert_eq!(board.in_flight[1].text, "the quarterly review");
        assert!(
            board.in_flight[1].at.is_empty(),
            "a line of the file carries no time of its own"
        );
    }

    #[test]
    fn a_scheduled_session_says_which_clock_is_running_it() {
        let mut running = session("s1", SessionState::Running);
        running.scheduled = Some(Scheduled {
            routine_id: "r1".to_owned(),
            routine_name: "Morning watch".to_owned(),
            skill: "watch.daily".to_owned(),
        });

        let sessions = [running];
        let board = assemble(facts(None, &sessions, &[], &[], Vec::new()));

        assert_eq!(board.in_flight[0].detail, "running — Morning watch");
    }

    #[test]
    fn the_projects_total_is_every_session_not_only_the_runs_in_the_window() {
        let mut first = session("s1", SessionState::Idle);
        first.cost = Cost {
            turns: 2,
            unreported: 0,
            prompt_tokens: 100,
            completion_tokens: 20,
            ..Cost::default()
        };
        let mut second = session("s2", SessionState::Idle);
        second.cost = Cost {
            turns: 1,
            unreported: 1,
            prompt_tokens: 0,
            completion_tokens: 0,
            ..Cost::default()
        };

        let sessions = [first, second];
        let board = assemble(facts(None, &sessions, &[], &[], Vec::new()));

        assert_eq!(board.cost.total(), 120);
        assert_eq!(board.cost.turns, 3);
        assert_eq!(board.cost.unreported, 1);
    }
}
