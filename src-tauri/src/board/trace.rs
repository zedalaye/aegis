//! Folding the audit log into runs (PLAN 7.3, Phase 17; PLAN 7.2, row 10).
//!
//! The log is one line per tool call. A person's question is never about a
//! line — it is *who ran, what did it cost, why did it fail* — and the unit
//! that answers it is a **run**: everything one piece of work did, wherever it
//! did it. This module is the fold from the first shape to the second, and it
//! is deliberately a pure function over entries and a ledger, so a replay can
//! be tested with no application, no clock and no model behind it.
//!
//! ## What ties lines into a run
//!
//! Nothing new is recorded to make this work. Every id it groups on was
//! already put on the line by the phase that introduced it — `agent_id` in 12,
//! `skill` in 13, `handoff` in 15, `routine` in 16 — which is what those
//! phases meant by *this cannot be reconstructed later*. [`RunRef::of`] reads
//! them in one order, widest first, so every line belongs to exactly one run:
//!
//! * **`handoff`** — a delegation. The only id that spans sessions: the Chief
//!   of Staff's own `handoff_delegate` line and every call every specialist
//!   then made carry it, under their own identities, in their own sessions.
//!   This is PLAN 7.2's "one run id covering CoS + specialists".
//! * **`routine`** — one firing of a clock, in the session it opened. The
//!   routine's id alone would collect a month of mornings into one row, so the
//!   session is part of the key: a routine has many runs, and each is one.
//! * **`skill`** — a runbook, in the session that ran it.
//! * neither — the conversation itself.
//!
//! The order is the containment order. A specialist running a runbook under a
//! brief is one delegation, not a delegation and a skill run; a scheduled run
//! always names the runbook it fired, and what a person wants to see is the
//! seven-o'clock run rather than the runbook's whole history.
//!
//! ## Why the cost comes from somewhere else
//!
//! Tokens are not a property of a tool call — a turn that called no tool spent
//! them too — so they are recorded on the session, per turn
//! ([`TurnCost`](crate::store::TurnCost)). The join is the `turn_id` that has
//! been on every audit line since Phase 4. [`fold`] takes the ledger beside the
//! entries and does the join once, which is also what scopes a board to one
//! project: a line whose session is not in the ledger is not this project's.
//!
//! One property is worth the arithmetic it costs, and there is a test for it:
//! **the runs of a session add up to the session.** A turn no run's lines
//! named — an ordinary reply that called nothing — still belongs to the
//! conversation, so it lands on that session's own run rather than nowhere.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::audit::{AuditDecision, AuditEntry, Outcome};
use crate::policy::tool;
use crate::store::{Cost, TurnCost};

/// Most runs [`fold`] returns.
///
/// The board is read top to bottom by a person; a list longer than this is one
/// nobody scrolls, and the window the log is read through is bounded anyway.
const RUNS_MAX: usize = 60;

/// Most artefact paths kept on one run.
///
/// A run that wrote two hundred files is a run whose *tools* tally is the
/// interesting number. The paths are here so a person can open what was
/// produced, which is a short list by nature.
const ARTEFACTS_MAX: usize = 24;

/// Longest reason kept from a line.
const REASON_MAX_CHARS: usize = 240;

// ---------------------------------------------------------------------------
// What a run is
// ---------------------------------------------------------------------------

/// What kind of work a run was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum RunKind {
    /// A delegation: a brief, and everything done under it.
    Handoff,
    /// One firing of a routine.
    Routine,
    /// One runbook, inside a conversation.
    Skill,
    /// The conversation itself.
    Session,
}

impl RunKind {
    /// The wire word, shared with the UI.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Handoff => "handoff",
            Self::Routine => "routine",
            Self::Skill => "skill",
            Self::Session => "session",
        }
    }
}

/// What ties a set of audit lines into one run.
///
/// A kind and an id rather than an enum with four payloads, because this is
/// also a map key and a value the UI hands back to ask for one run again.
/// `session_id` is part of the key for everything except a delegation, which
/// is the one kind that spans sessions on purpose.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct RunRef {
    /// What kind of work it was.
    pub kind: RunKind,
    /// The id the lines share: the delegation, the routine, the runbook's
    /// name, or the session.
    pub id: String,
    /// The session it happened in. Empty for a delegation.
    pub session_id: String,
}

impl RunRef {
    /// The run one audit line belongs to.
    ///
    /// Total: every line belongs somewhere, and a line carrying none of the
    /// three ids belongs to its conversation.
    pub fn of(entry: &AuditEntry) -> Self {
        if !entry.handoff.is_empty() {
            return Self {
                kind: RunKind::Handoff,
                id: entry.handoff.clone(),
                session_id: String::new(),
            };
        }
        if !entry.routine.is_empty() {
            return Self {
                kind: RunKind::Routine,
                id: entry.routine.clone(),
                session_id: entry.session_id.clone(),
            };
        }
        if !entry.skill.is_empty() {
            return Self {
                kind: RunKind::Skill,
                id: entry.skill.clone(),
                session_id: entry.session_id.clone(),
            };
        }
        Self::session(&entry.session_id)
    }

    /// The run that is a whole conversation.
    pub fn session(session_id: &str) -> Self {
        Self {
            kind: RunKind::Session,
            id: session_id.to_owned(),
            session_id: session_id.to_owned(),
        }
    }
}

/// How a run ended, in the vocabulary `COS.md` already uses.
///
/// Four words plus one. The first four are a report's own — a run that
/// returned says how it went, and nothing here second-guesses it.
/// [`Ran`](RunStatus::Ran) is the fifth, and it is not a failure: it is work
/// that made calls and never filed a report, which is what an ordinary
/// conversation is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum RunStatus {
    /// It reported `done`.
    Done,
    /// It reported `blocked`: something it needed was not there.
    Blocked,
    /// It reported `needs_you`: a person has to decide.
    NeedsYou,
    /// It did not report, and something in it was refused or failed.
    Failed,
    /// It did not report, and nothing went wrong.
    Ran,
}

impl RunStatus {
    /// The wire word, shared with the UI.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::NeedsYou => "needs_you",
            Self::Failed => "failed",
            Self::Ran => "ran",
        }
    }

    /// Whether this is a run that stopped short.
    ///
    /// What puts a line in the board's *Blocked* column. `needs_you` is not
    /// here: it is not blocked, it is waiting on a person, which is the
    /// *Attention* column.
    pub const fn is_stuck(self) -> bool {
        matches!(self, Self::Blocked | Self::Failed)
    }

    /// The status word a report used, as the tool wrote it.
    fn from_report(word: &str) -> Option<Self> {
        match word {
            "done" => Some(Self::Done),
            "blocked" => Some(Self::Blocked),
            "needs_you" => Some(Self::NeedsYou),
            _ => None,
        }
    }
}

/// How many times one tool was called in a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct Tally {
    /// Tool name.
    pub tool: String,
    /// How many calls.
    pub calls: u32,
}

/// One run, as the board lists it and the detail pane reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct Run {
    /// What ties it together.
    pub run: RunRef,
    /// What to call it: a brief's goal, a routine's name, a runbook's name, a
    /// conversation's title. Falls back to the id, which is never nothing.
    pub label: String,
    /// The first thing it did, RFC3339 UTC.
    pub started_at: String,
    /// The last, RFC3339 UTC.
    pub ended_at: String,
    /// The identities that ran under it, in the order they first appear.
    ///
    /// More than one only for a delegation, which is the point of that kind:
    /// "who ran" is a list when a Chief of Staff hands work to two specialists.
    pub agents: Vec<String>,
    /// The sessions it touched, likewise. One click each.
    pub sessions: Vec<String>,
    /// The runbook it followed, when the run is not itself one.
    pub skill: String,
    /// Every tool call it made, refused and failed ones included.
    pub calls: u32,
    /// How many a person was asked about.
    pub asked: u32,
    /// How many never ran, because policy or a person refused them.
    pub denied: u32,
    /// How many ran and failed.
    pub failed: u32,
    /// Which tools, and how often. Most-called first.
    pub tools: Vec<Tally>,
    /// What it left on disk: files written, captures taken, artefacts a report
    /// named. Paths, never contents — the log never held the contents.
    pub artefacts: Vec<String>,
    /// How it ended.
    pub status: RunStatus,
    /// Why, in the words the record already used. Empty when there is nothing
    /// to explain.
    pub reason: String,
    /// Tool execution time, summed over the calls.
    ///
    /// Not the wall clock: a run parked for five minutes on an approval dialog
    /// did not spend five minutes working, and the two numbers differing is
    /// usually the interesting part. The wall clock is `ended_at` less
    /// `started_at`, which the UI has both halves of.
    #[ts(type = "number")]
    pub tool_ms: u64,
    /// What it spent.
    pub cost: Cost,
}

impl Run {
    /// An empty run of this reference.
    fn new(run: RunRef) -> Self {
        let label = run.id.clone();
        Self {
            run,
            label,
            started_at: String::new(),
            ended_at: String::new(),
            agents: Vec::new(),
            sessions: Vec::new(),
            skill: String::new(),
            calls: 0,
            asked: 0,
            denied: 0,
            failed: 0,
            tools: Vec::new(),
            artefacts: Vec::new(),
            status: RunStatus::Ran,
            reason: String::new(),
            tool_ms: 0,
            cost: Cost::default(),
        }
    }
}

/// One run, and the lines it is replayed from.
///
/// The replay of PLAN 7.2 row 10, and it is deliberately not a rendering: the
/// entries are the audit lines themselves, in the order they happened, so what
/// the pane shows is what is on disk rather than a story assembled about it.
/// A person reading this is checking the record against the transcript, and a
/// record that had been prettied up first would be worth less than the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct RunTrace {
    /// The run, as the board lists it.
    pub run: Run,
    /// Its lines, oldest first.
    pub entries: Vec<AuditEntry>,
}

// ---------------------------------------------------------------------------
// What the fold is given besides the log
// ---------------------------------------------------------------------------

/// One session, as a fold needs to see it.
///
/// Three jobs in one argument, which is why it is not three arguments. It
/// **scopes** the fold — a line whose session is not here belongs to another
/// project, or to a conversation that has since been deleted, and either way it
/// is not on this board. It carries the **cost**, which is the join the log
/// cannot make on its own. And it carries the **names**, so a fold can label a
/// run without reaching into three stores.
#[derive(Debug, Clone, Default)]
pub struct SessionLedger {
    /// The session.
    pub session_id: String,
    /// Its title — what a conversation's run is called.
    pub title: String,
    /// The name of the routine that opened it, when a clock did.
    pub routine: String,
    /// The delegation that opened it, when a brief did.
    pub handoff: String,
    /// Whether a turn is in flight in it right now.
    ///
    /// Only [`settle`] reads it, and only to keep from calling work that has
    /// not finished a failure: a runbook between its `skill_run` and its
    /// `skill_return` looks exactly like one that stopped without reporting,
    /// and the difference is a fact about this process rather than about the
    /// log.
    pub running: bool,
    /// What each of its turns spent, in any order.
    pub turns: Vec<TurnCost>,
}

// ---------------------------------------------------------------------------
// The fold
// ---------------------------------------------------------------------------

/// What a run needs while it is being folded, and does not carry afterwards.
///
/// Beside the run rather than on it: a tally is a map until it is ordered at
/// the end, the turns are a set until they are charged, and "the latest report"
/// is a comparison rather than a field a half-built row should expose.
#[derive(Debug, Default)]
struct Working {
    /// Which tools, and how often.
    tools: HashMap<String, u32>,
    /// Every `(session, turn)` this run's lines named.
    turns: BTreeSet<(String, String)>,
    /// The latest report it filed, if any.
    report: Option<AuditEntry>,
    /// The latest thing that went wrong in it, if any.
    trouble: Option<AuditEntry>,
}

/// Folds audit lines and a ledger into runs, newest first.
///
/// Entries may arrive in any order — [`AuditLog::tail`](crate::audit::AuditLog::tail)
/// hands them back newest first, a file read hands them back oldest first — so
/// nothing here depends on it: every span is a minimum and a maximum over
/// timestamps, and the report a run's status comes from is the latest one, not
/// the last one seen.
///
/// A line whose session is not in `ledger` is dropped. That is the scoping
/// rule, and it is also why a deleted session's calls leave a board while
/// staying in the log: the log is the record of what was done, the board is a
/// view of one project's work, and the second cannot invent a project for a
/// session that no longer names one.
pub fn fold(entries: &[AuditEntry], ledger: &[SessionLedger]) -> Vec<Run> {
    let known: HashMap<&str, &SessionLedger> = ledger
        .iter()
        .map(|session| (session.session_id.as_str(), session))
        .collect();

    let mut folding: Vec<(Run, Working)> = Vec::new();
    let mut at: HashMap<RunRef, usize> = HashMap::new();

    for entry in entries {
        if !known.contains_key(entry.session_id.as_str()) {
            continue;
        }

        let index = open(&mut folding, &mut at, RunRef::of(entry));
        let (run, working) = &mut folding[index];

        absorb(run, entry);
        *working.tools.entry(entry.tool.clone()).or_insert(0) += 1;
        working
            .turns
            .insert((entry.session_id.clone(), entry.turn_id.clone()));

        if is_report(entry) && later(working.report.as_ref(), entry) {
            working.report = Some(entry.clone());
        }
        if is_trouble(entry) && later(working.trouble.as_ref(), entry) {
            working.trouble = Some(entry.clone());
        }
    }

    // Every turn a run's lines named, so the ones nobody named can be found.
    // Owned rather than borrowed: the loop below opens runs, and a set holding
    // references into the vector it is about to grow would not survive it.
    let claimed: HashSet<(String, String)> = folding
        .iter()
        .flat_map(|(_, working)| working.turns.iter().cloned())
        .collect();

    // A turn that called no tool is invisible to the log and still cost
    // something. It belongs to its conversation, so it lands on that session's
    // own run — opened here when the session made no unclaimed calls at all,
    // which is what a chat that only talked looks like.
    for session in ledger {
        let unclaimed: Vec<&TurnCost> = session
            .turns
            .iter()
            .filter(|turn| !claimed.contains(&(session.session_id.clone(), turn.turn_id.clone())))
            .collect();
        if unclaimed.is_empty() {
            continue;
        }

        let index = open(&mut folding, &mut at, RunRef::session(&session.session_id));
        let (run, working) = &mut folding[index];

        if !run.sessions.iter().any(|held| held == &session.session_id) {
            run.sessions.push(session.session_id.clone());
        }
        for turn in unclaimed {
            working
                .turns
                .insert((session.session_id.clone(), turn.turn_id.clone()));
            span(run, &turn.at);
        }
    }

    let mut runs: Vec<Run> = folding
        .into_iter()
        .map(|(mut run, working)| {
            run.tools = order(working.tools);
            run.cost = charge(&working.turns, &known);
            let running = run
                .sessions
                .iter()
                .any(|id| known.get(id.as_str()).is_some_and(|held| held.running));
            settle(
                &mut run,
                running,
                working.report.as_ref(),
                working.trouble.as_ref(),
            );
            label(&mut run, ledger);
            run
        })
        .collect();

    // Newest first, by what the run last did. Ties break on the label, so two
    // runs that finished in the same millisecond do not swap places between
    // two reads of the same board.
    runs.sort_by(|left, right| {
        right
            .ended_at
            .cmp(&left.ended_at)
            .then_with(|| left.label.cmp(&right.label))
    });
    runs.truncate(RUNS_MAX);
    runs
}

/// The index of a run, opening an empty one when it is new.
fn open(
    folding: &mut Vec<(Run, Working)>,
    at: &mut HashMap<RunRef, usize>,
    reference: RunRef,
) -> usize {
    if let Some(index) = at.get(&reference) {
        return *index;
    }

    folding.push((Run::new(reference.clone()), Working::default()));
    let index = folding.len() - 1;
    at.insert(reference, index);
    index
}

/// Adds one line's facts to the run it belongs to.
fn absorb(run: &mut Run, entry: &AuditEntry) {
    span(run, &entry.ts);

    if !entry.agent_id.is_empty() && !run.agents.iter().any(|held| held == &entry.agent_id) {
        run.agents.push(entry.agent_id.clone());
    }
    if !run.sessions.iter().any(|held| held == &entry.session_id) {
        run.sessions.push(entry.session_id.clone());
    }
    // The runbook a run *followed*, when the run is not itself one: a scheduled
    // run always names its skill, and a delegated specialist may.
    if run.run.kind != RunKind::Skill && run.skill.is_empty() && !entry.skill.is_empty() {
        run.skill = entry.skill.clone();
    }

    run.calls = run.calls.saturating_add(1);
    run.tool_ms = run.tool_ms.saturating_add(entry.duration_ms);

    if matches!(
        entry.decision,
        AuditDecision::AllowOnce | AuditDecision::AllowSession
    ) {
        run.asked = run.asked.saturating_add(1);
    }
    match entry.outcome {
        Outcome::Denied => run.denied = run.denied.saturating_add(1),
        Outcome::Error => run.failed = run.failed.saturating_add(1),
        Outcome::Ok | Outcome::Cancelled => {}
    }

    for path in produced(entry) {
        if run.artefacts.len() < ARTEFACTS_MAX && !run.artefacts.iter().any(|held| held == &path) {
            run.artefacts.push(path);
        }
    }
}

/// Widens a run's span to include one timestamp.
fn span(run: &mut Run, ts: &str) {
    if ts.is_empty() {
        return;
    }
    if run.started_at.is_empty() || ts < run.started_at.as_str() {
        run.started_at = ts.to_owned();
    }
    if ts > run.ended_at.as_str() {
        run.ended_at = ts.to_owned();
    }
}

/// The files one line says were produced.
///
/// Three places, because three things leave one. `screen_capture` records its
/// file on the line itself; `fs_write` names the path it replaced in its
/// arguments, which the log keeps whole for exactly this kind of reading; and
/// a report names what the work produced, which is the only one of the three
/// that says a file *mattered* rather than that it changed.
fn produced(entry: &AuditEntry) -> Vec<String> {
    if entry.outcome != Outcome::Ok {
        return Vec::new();
    }

    let mut out = Vec::new();
    if let Some(artifact) = &entry.artifact {
        out.push(artifact.path.clone());
    }

    let Ok(args) = serde_json::from_str::<serde_json::Value>(&entry.args_redacted) else {
        return out;
    };

    if entry.tool == tool::FS_WRITE {
        if let Some(path) = args.get("path").and_then(serde_json::Value::as_str) {
            out.push(path.to_owned());
        }
    }
    if is_report(entry) {
        if let Some(named) = args.get("artefacts").and_then(serde_json::Value::as_array) {
            out.extend(
                named
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned),
            );
        }
    }
    out
}

/// Whether a line is a runbook or a brief filing its report.
fn is_report(entry: &AuditEntry) -> bool {
    entry.outcome == Outcome::Ok
        && (entry.tool == tool::SKILL_RETURN || entry.tool == tool::HANDOFF_RETURN)
}

/// Whether a line is one somebody would want explained.
fn is_trouble(entry: &AuditEntry) -> bool {
    matches!(entry.outcome, Outcome::Denied | Outcome::Error)
}

/// Whether `entry` happened no earlier than what is already held.
fn later(held: Option<&AuditEntry>, entry: &AuditEntry) -> bool {
    held.is_none_or(|held| entry.ts >= held.ts)
}

/// Decides how a run ended, and why.
///
/// Three rules, in order.
///
/// **A report wins over trouble, always.** A runbook that was refused a write,
/// coped, and returned `done` is done — the refusal is on its record as a
/// number, and re-reading it as a failure would be the harness overruling the
/// only thing in the loop that knew what the work needed.
///
/// **A conversation cannot fail.** Failure needs a promise, and only three of
/// the four kinds make one. A denial in a chat is the person's own answer and
/// the turn carries on by design (PLAN 3.1); a board that read it as a failed
/// conversation would be calling the approval gate working as intended a
/// problem. What went wrong inside one is on the row as counts, and in the
/// replay line by line.
///
/// **Work that has not finished has not failed.** A runbook between its
/// `skill_run` and its `skill_return` is indistinguishable in the log from one
/// that stopped without reporting; the session's live state is what tells them
/// apart, and it is the one thing here that does not come from the record.
fn settle(run: &mut Run, running: bool, report: Option<&AuditEntry>, trouble: Option<&AuditEntry>) {
    if let Some(report) = report {
        let args = serde_json::from_str::<serde_json::Value>(&report.args_redacted).ok();
        let word = args
            .as_ref()
            .and_then(|args| args.get("status"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();

        if let Some(status) = RunStatus::from_report(word) {
            run.status = status;
            run.reason = match status {
                RunStatus::Done => String::new(),
                _ => args
                    .as_ref()
                    .and_then(|args| {
                        args.get("open_questions")
                            .and_then(serde_json::Value::as_str)
                            .or_else(|| args.get("summary").and_then(serde_json::Value::as_str))
                    })
                    .map(shorten)
                    .unwrap_or_default(),
            };
            return;
        }
    }

    if run.run.kind == RunKind::Session || running {
        return;
    }

    // A brief, a runbook or a firing that never reported. That is a silence,
    // and `COS.md` is clear that a silence is not an answer — so it is a
    // failure whether or not anything visible went wrong, and the last thing
    // that did is the best explanation there is.
    run.status = RunStatus::Failed;
    run.reason = match trouble {
        Some(trouble) => shorten(&match trouble.outcome {
            Outcome::Denied => format!("{} refused: {}", trouble.tool, trouble.policy_reason),
            _ => match &trouble.error_code {
                Some(code) => format!("{} failed: {code}", trouble.tool),
                None => format!("{} failed", trouble.tool),
            },
        }),
        None => "it ended without reporting".to_owned(),
    };
}

/// Names a run from the sessions it touched.
fn label(run: &mut Run, ledger: &[SessionLedger]) {
    let named = match run.run.kind {
        // A specialist's session is titled from the brief's goal, which is the
        // best name a delegation has. The Chief of Staff's own session is in
        // `sessions` too, so the delegated one is picked deliberately rather
        // than by taking the first.
        RunKind::Handoff => ledger
            .iter()
            .find(|session| session.handoff == run.run.id)
            .map(|session| session.title.clone()),
        RunKind::Routine => ledger
            .iter()
            .find(|session| session.session_id == run.run.session_id)
            .map(|session| session.routine.clone()),
        RunKind::Session => ledger
            .iter()
            .find(|session| session.session_id == run.run.id)
            .map(|session| session.title.clone()),
        // A runbook's name is already the best name it has.
        RunKind::Skill => None,
    };

    if let Some(named) = named.filter(|named| !named.trim().is_empty()) {
        run.label = named;
    }
}

/// What a set of turns spent, from the ledger they belong to.
fn charge(turns: &BTreeSet<(String, String)>, known: &HashMap<&str, &SessionLedger>) -> Cost {
    let mut cost = Cost::default();
    for (session_id, turn_id) in turns {
        let Some(session) = known.get(session_id.as_str()) else {
            continue;
        };
        if let Some(turn) = session.turns.iter().find(|turn| &turn.turn_id == turn_id) {
            cost.add(Cost::of([turn]));
        }
    }
    cost
}

/// The tally, most-called first, then alphabetical.
fn order(counted: HashMap<String, u32>) -> Vec<Tally> {
    let mut tally: Vec<Tally> = counted
        .into_iter()
        .map(|(tool, calls)| Tally { tool, calls })
        .collect();
    tally.sort_by(|left, right| {
        right
            .calls
            .cmp(&left.calls)
            .then_with(|| left.tool.cmp(&right.tool))
    });
    tally
}

/// Caps a reason at something a row can hold.
fn shorten(text: &str) -> String {
    let text = text.trim();
    let mut kept: String = text.chars().take(REASON_MAX_CHARS).collect();
    if kept.chars().count() < text.chars().count() {
        kept.push('…');
    }
    kept
}

#[cfg(test)]
mod tests {
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
            "artefacts": ["artefacts/tickets.md"],
        })
        .to_string();

        let runs = fold(&[refused, report], &[ledger("s1", &["t1"])]);

        let run = find(&runs, RunKind::Skill, "inbox.triage");
        assert_eq!(run.status, RunStatus::Done);
        assert_eq!(run.denied, 1, "the refusal is still on the record");
        assert!(run.reason.is_empty());
        assert_eq!(run.artefacts, ["artefacts/tickets.md"]);
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
            json!({ "path": "artefacts/report.md", "content": "<40 bytes>" }).to_string();
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
            "artefacts": ["artefacts/report.md", "artefacts/summary.md"],
        })
        .to_string();

        let runs = fold(&[wrote, captured, report], &[ledger("s1", &["t1"])]);

        let run = find(&runs, RunKind::Session, "s1");
        assert_eq!(
            run.artefacts,
            [
                "artefacts/report.md",
                "captures/capture-1.png",
                "artefacts/summary.md"
            ],
            "each path once, in the order it was produced"
        );
    }

    #[test]
    fn a_refused_call_produced_nothing() {
        let mut refused = line("s1", "t1", "fs_write");
        refused.outcome = Outcome::Denied;
        refused.args_redacted = json!({ "path": "artefacts/never.md" }).to_string();

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
}
