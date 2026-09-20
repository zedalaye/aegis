//! Folding the audit log into runs (PLAN 7.3, Phase 17; PLAN 7.2, row 10).
//!
//! A pure fold from one-line-per-call audit entries to **runs**: who ran, what
//! it cost, why it failed.
//!
//! [`RunRef::of`] groups each line by the widest id it carries (containment
//! order):
//!
//! * **`handoff`** — a delegation, spanning the CoS's and specialists' sessions
//!   (PLAN 7.2, row 10);
//! * **`routine`** + session — one firing;
//! * **`skill`** + session — one runbook run;
//! * otherwise the conversation itself.
//!
//! Cost joins from the session ledger on `turn_id`
//! ([`TurnCost`](crate::store::TurnCost)); the ledger also scopes the fold to
//! one project. A turn with no tool calls lands on its conversation's run, so a
//! session's runs add up to the session.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::audit::{AuditDecision, AuditEntry, Outcome};
use crate::policy::tool;
use crate::store::{Cost, TurnCost};

/// Most runs [`fold`] returns.
const RUNS_MAX: usize = 60;

/// Most artefact paths kept on one run.
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
/// Also a map key and the value the UI sends back. `session_id` is part of the
/// key except for a delegation, which spans sessions.
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
    /// The run one audit line belongs to; without any run id, its conversation.
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
/// A report's own statuses, plus [`Ran`](RunStatus::Ran): calls made with no
/// report, like an ordinary conversation.
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

    /// Whether this run stopped short (the *Blocked* column; `needs_you` is
    /// *Attention*).
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
    /// The identities that ran under it, in first-appearance order.
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
    /// Tool execution time summed over calls — not wall clock, which excludes
    /// waiting on approvals.
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
/// The replay of PLAN 7.2 row 10: the raw audit entries, in order.
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
/// It scopes the fold, carries the per-turn cost, and names runs.
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
    /// Whether a turn is in flight, so [`settle`] does not call unfinished work
    /// a failure.
    pub running: bool,
    /// What each of its turns spent, in any order.
    pub turns: Vec<TurnCost>,
}

// ---------------------------------------------------------------------------
// The fold
// ---------------------------------------------------------------------------

/// Scratch state for a run while it is being folded.
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
/// Order-independent: spans are min/max timestamps, and status comes from the
/// latest report. Lines whose session is not in `ledger` are dropped.
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

    // Turns absent from the log still cost: charge them to the session's
    // conversation run, opening it if needed.
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
/// From a capture's artifact, an `fs_write` path, or a report's `artefacts`.
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
/// In order: a report always wins over refusals or errors; a conversation
/// never fails (denials are answers, PLAN 3.1); and work still running has not
/// failed.
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

    // A brief, runbook or firing that never reported: silence is a failure.
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
        // Named after the delegated session (titled from the goal), not the
        // CoS's own.
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
mod tests;
