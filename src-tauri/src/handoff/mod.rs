//! The two halves of a delegation: the brief that goes out, the report that
//! comes back (`COS.md` *Handoff*; PLAN 7.3, Phases 13 and 15).
//!
//! ```text
//! goal:                            status: done | blocked | needs_you
//! owner:                           summary:            # five lines max
//! priority:                        artefacts:          # paths
//! inputs:      # paths, not paste   evidence:           # tests, diff
//! constraints:                     open_questions:
//! definition_of_done:              next_owner:
//! approval_needed:
//! return_format:
//! ```
//!
//! [`Brief`] (left, Phase 15) goes out through [`bus`]; [`Report`] (right,
//! Phase 13) comes back from a skill or a delegated run. One module, because a
//! brief's `return_format` and a report's `next_owner` are one contract.
//!
//! Validation is structural, not a quality judgement (PLAN 7.6):
//!
//! * **Out** ([`check_brief`]): inputs must be paths or links, never pasted
//!   prose; required fields and caps.
//! * **Back** ([`check`]): a `done` must name an existing artefact or some
//!   evidence; `blocked` / `needs_you` must carry a question; `COS.md`'s caps.

pub mod bus;
pub mod runner;

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

// ---------------------------------------------------------------------------
// Out: the brief
// ---------------------------------------------------------------------------

/// How soon a brief wants attention: three words, not an uncalibrated number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum Priority {
    /// Someone is blocked on it, or a deadline is close.
    High,
    /// Ordinary work.
    Normal,
    /// Worth doing, nothing waits on it.
    Low,
}

impl Priority {
    /// The wire string, which is also what the model wrote.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Normal => "normal",
            Self::Low => "low",
        }
    }
}

/// What the brief asks to come back (`COS.md`: `status | artefact | question`).
///
/// Every return is still a [`Report`]; this changes what a complete one must
/// contain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum ReturnFormat {
    /// A status line; nothing is expected on disk.
    Status,
    /// A file, named in `artefacts`.
    Artefact,
    /// An answer to something the CoS could not settle.
    Question,
}

impl ReturnFormat {
    /// The wire string.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Artefact => "artefact",
            Self::Question => "question",
        }
    }

    /// What this asks of the owner, ending with what a `done` must point at so
    /// [`check`] does not cost a round.
    pub const fn expectation(self) -> &'static str {
        match self {
            Self::Status => {
                "A status: say what is true now. Nothing is expected on disk — so if you finish \
                 `done`, `evidence` is what makes it checkable: name the file you read or the \
                 command you ran. A `done` that points at nothing is refused."
            }
            Self::Artefact => {
                "An artefact: the work is a file in the workspace, and the return names its path \
                 in `artefacts`. The path is checked — a `done` naming a file that is not there \
                 is refused."
            }
            Self::Question => {
                "An answer: the question in the goal is what this is for, and it goes in \
                 `summary`. Name what you read to answer it in `evidence`; a `done` that points \
                 at nothing is refused."
            }
        }
    }
}

/// Longest `goal`, and longest `definition_of_done`.
///
/// A goal that does not fit on a couple of lines is two briefs.
pub const GOAL_MAX_CHARS: usize = 400;

/// Most entries in `inputs` or in `constraints`.
pub const BRIEF_LIST_MAX: usize = 12;

/// Longest one input or constraint: room for a path or URL, not a paragraph.
pub const BRIEF_ENTRY_MAX_CHARS: usize = 240;

/// A delegation, as it goes out (`COS.md` *Handoff*).
///
/// Everything the owner is given — deliberately no transcript. `owner` is an
/// identity name or id, resolved by [`bus`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Brief {
    /// What is to be achieved, in a line or two.
    pub goal: String,
    /// The identity that is to do it, by name or id.
    pub owner: String,
    /// How soon.
    pub priority: Priority,
    /// Paths and URLs the work starts from. Never pasted text.
    pub inputs: Vec<String>,
    /// What it must not do, and what it must respect.
    pub constraints: Vec<String>,
    /// How the owner can tell it is finished.
    pub definition_of_done: String,
    /// What is expected to stop for a human. Informational: it grants nothing.
    pub approval_needed: String,
    /// What kind of return is expected.
    pub return_format: ReturnFormat,
}

/// The delegated run a turn *is*, and where its return lands: a per-turn cell
/// the runner creates, `handoff_return` fills, and the runner reads after. The
/// mutex only gives interior mutability.
#[derive(Debug)]
pub struct Open {
    /// The delegation this run belongs to. Reaches every audit line it writes.
    id: String,
    /// What was returned, once something was.
    returned: Mutex<Option<Report>>,
}

impl Open {
    /// A run of `id`, with nothing returned yet.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            returned: Mutex::new(None),
        }
    }

    /// The delegation this run belongs to.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Records an accepted return; the last one wins.
    pub fn close(&self, report: Report) {
        *self.lock() = Some(report);
    }

    /// Whether this run has answered; the turn loop stops once it has.
    pub fn closed(&self) -> bool {
        self.lock().is_some()
    }

    /// Takes the return, leaving the run empty.
    pub fn take(&self) -> Option<Report> {
        self.lock().take()
    }

    /// Locks the cell, recovering from poison: it cannot be left torn.
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Report>> {
        self.returned
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// One delegation: the briefs, and the separate review that fans in after them
/// if anything came back (`COS.md` *Loop*).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The briefs that go out together.
    pub briefs: Vec<Brief>,
    /// The brief the reviewer gets afterwards, when one was asked for.
    pub review: Option<Brief>,
}

/// Judges a brief, and renders the one the owner will be handed.
///
/// `Ok` is the brief rendered in `COS.md`'s shape (filed and used to open the
/// owner's session); `Err` is written for the model to fix it.
pub fn check_brief(brief: &Brief) -> Result<String, String> {
    let goal = brief.goal.trim();
    if goal.is_empty() {
        return Err(
            "`goal` is what the brief is for — say what is to be achieved. A delegation with no \
             goal is a conversation, and the point of a brief is that it is not one"
                .to_owned(),
        );
    }
    if goal.chars().count() > GOAL_MAX_CHARS {
        return Err(format!(
            "`goal` is longer than {GOAL_MAX_CHARS} characters. A goal that does not fit is two \
             briefs — split it, or put the detail in a file and name it in `inputs`"
        ));
    }

    if brief.owner.trim().is_empty() {
        return Err(
            "`owner` names the identity that will do this. Work with no owner is work nobody \
             picks up"
                .to_owned(),
        );
    }

    let done = brief.definition_of_done.trim();
    if done.is_empty() {
        return Err(
            "`definition_of_done` is how the owner knows to stop. Without it, the return is \
             somebody's opinion that enough has happened"
                .to_owned(),
        );
    }
    if done.chars().count() > GOAL_MAX_CHARS {
        return Err(format!(
            "`definition_of_done` is longer than {GOAL_MAX_CHARS} characters. It is a test, not a \
             specification — the specification goes in a file `inputs` names"
        ));
    }

    check_inputs(&brief.inputs)?;
    brief_entries("constraints", &brief.constraints)?;

    Ok(render_brief(brief, goal, done))
}

/// Checks `inputs` are paths or links: a line break or excessive length means
/// pasted prose.
fn check_inputs(values: &[String]) -> Result<(), String> {
    for value in values {
        if value.contains('\n') {
            return Err(format!(
                "an entry in `inputs` spans several lines, so it is text rather than a reference. \
                 Inputs are paths and URLs: write the text to a file with `fs_write` and name that \
                 path here. `{}…` is not a path",
                value.chars().take(40).collect::<String>().trim()
            ));
        }
    }
    brief_entries("inputs", values)
}

/// One brief list's length, and the length of what is in it.
fn brief_entries(field: &str, values: &[String]) -> Result<(), String> {
    for value in values {
        if value.trim().is_empty() {
            return Err(format!(
                "`{field}` has an empty entry — drop it rather than send it"
            ));
        }
        if value.chars().count() > BRIEF_ENTRY_MAX_CHARS {
            return Err(format!(
                "an entry in `{field}` is longer than {BRIEF_ENTRY_MAX_CHARS} characters, so it is \
                 not a reference. Put it in a file and name the path"
            ));
        }
    }

    if values.len() > BRIEF_LIST_MAX {
        return Err(format!(
            "`{field}` has {} entries and {BRIEF_LIST_MAX} is the limit — a brief that needs more \
             is really several",
            values.len()
        ));
    }
    Ok(())
}

/// The brief rendered in `COS.md`'s shape, readable by the owner and a person.
fn render_brief(brief: &Brief, goal: &str, done: &str) -> String {
    let mut out = String::new();

    let _ = writeln!(out, "goal: {goal}");
    let _ = writeln!(out, "owner: {}", brief.owner.trim());
    let _ = writeln!(out, "priority: {}", brief.priority.as_str());
    list(&mut out, "inputs", brief.inputs.iter().map(String::as_str));
    list(
        &mut out,
        "constraints",
        brief.constraints.iter().map(String::as_str),
    );
    let _ = writeln!(out, "definition_of_done: {done}");

    let approval = brief.approval_needed.trim();
    let _ = writeln!(
        out,
        "approval_needed: {}",
        if approval.is_empty() { "—" } else { approval }
    );
    let _ = writeln!(out, "return_format: {}", brief.return_format.as_str());

    out
}

// ---------------------------------------------------------------------------
// Back: the report
// ---------------------------------------------------------------------------

/// How a delegated or skill run ended (`COS.md` *Handoff*). "Partly done" is
/// `needs_you` with open questions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum Status {
    /// The definition of done is met, and the artefacts say so.
    Done,
    /// A source the runbook needed was missing, or a step could not run.
    Blocked,
    /// It needs a decision, an approval, or something only the human has.
    NeedsYou,
}

impl Status {
    /// The wire string, which is also what the model wrote.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::NeedsYou => "needs_you",
        }
    }
}

/// Most lines a summary may have (`COS.md`: "five lines max").
pub const SUMMARY_MAX_LINES: usize = 5;

/// Most characters a summary may have, alongside the line cap.
pub const SUMMARY_MAX_CHARS: usize = 800;

/// Most entries in any one of the lists.
pub const LIST_MAX: usize = 12;

/// Most characters in one list entry.
pub const ENTRY_MAX_CHARS: usize = 240;

/// Longest `next_owner`.
const OWNER_MAX_CHARS: usize = 64;

/// One artefact the run produced: the path as written (quoted in messages) and
/// as resolved (checked).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artefact {
    /// The path as the model wrote it.
    pub shown: String,
    /// The same path, resolved and contained by policy.
    pub path: PathBuf,
}

/// A return as the model wrote it, before policy has resolved anything.
///
/// `artefacts` are still raw strings; [`policy`](crate::policy) resolves them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    /// How the run ended.
    pub status: Status,
    /// Five lines at most, saying what happened.
    pub summary: String,
    /// What the run produced, as the model named it.
    pub artefacts: Vec<String>,
    /// What backs the claim: a test, a diff, a capture.
    pub evidence: Vec<String>,
    /// What the next owner has to answer.
    pub open_questions: Vec<String>,
    /// Who should pick it up. May be empty.
    pub next_owner: String,
}

/// A return, parsed and with its paths resolved, before its content is judged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// How the run ended.
    pub status: Status,
    /// Five lines at most, saying what happened.
    pub summary: String,
    /// What the run produced, as paths inside the workspace.
    pub artefacts: Vec<Artefact>,
    /// What backs the claim: a test, a diff, a capture.
    pub evidence: Vec<String>,
    /// What the next owner has to answer.
    pub open_questions: Vec<String>,
    /// Who should pick it up. May be empty.
    pub next_owner: String,
}

/// Judges a return, and renders the one the runtime will record.
///
/// `Ok` is the normalized report text (tool content, read at fan-in); `Err`
/// names the broken rule and what would pass.
pub fn check(report: &Report) -> Result<String, String> {
    let summary = report.summary.trim();
    if summary.is_empty() {
        return Err(
            "`summary` is what the run is *for* — say in a line or two what happened. \
                    A return with no summary is a status nobody can read"
                .to_owned(),
        );
    }
    if summary.lines().count() > SUMMARY_MAX_LINES {
        return Err(format!(
            "`summary` is {} lines; {SUMMARY_MAX_LINES} is the limit. Anything longer belongs in \
             an artefact this return points at",
            summary.lines().count()
        ));
    }
    if summary.chars().count() > SUMMARY_MAX_CHARS {
        return Err(format!(
            "`summary` is longer than {SUMMARY_MAX_CHARS} characters. It is a status, not a \
             transcript — put the detail in an artefact and name its path"
        ));
    }

    entries(
        "artefacts",
        report.artefacts.iter().map(|a| a.shown.as_str()),
    )?;
    entries("evidence", report.evidence.iter().map(String::as_str))?;
    entries(
        "open_questions",
        report.open_questions.iter().map(String::as_str),
    )?;

    if report.next_owner.chars().count() > OWNER_MAX_CHARS {
        return Err(format!(
            "`next_owner` is a name, not a description — keep it under {OWNER_MAX_CHARS} \
             characters"
        ));
    }

    match report.status {
        Status::Done => {
            // The commonest way a model claims a file it never wrote. One
            // `metadata` call turns a plausible paragraph into a refusal it
            // can do something about.
            if let Some(missing) = report
                .artefacts
                .iter()
                .find(|artefact| !artefact.path.is_file())
            {
                return Err(format!(
                    "`{}` is not a file. A run is not `done` while an artefact it names is not \
                     on disk — write it, or return `blocked` and say why you could not",
                    missing.shown
                ));
            }
            if report.artefacts.is_empty() && report.evidence.is_empty() {
                return Err(
                    "a `done` has to point at something: a path in `artefacts`, or a \
                            test, diff or capture in `evidence`. A done with nothing to point \
                            at cannot be checked afterwards and cannot be replayed"
                        .to_owned(),
                );
            }
        }
        Status::Blocked | Status::NeedsYou => {
            if report.open_questions.is_empty() {
                return Err(format!(
                    "a `{}` needs at least one `open_questions` entry — say what you need, and \
                     from whom. Escalating with nothing to answer is a dead end for whoever it \
                     reaches",
                    report.status.as_str()
                ));
            }
        }
    }

    Ok(render(report, summary))
}

/// Checks one list's length and the length of what is in it.
fn entries<'a>(field: &str, values: impl Iterator<Item = &'a str>) -> Result<(), String> {
    let mut count = 0usize;

    for value in values {
        count += 1;
        if value.trim().is_empty() {
            return Err(format!(
                "`{field}` has an empty entry — drop it rather than send it"
            ));
        }
        if value.chars().count() > ENTRY_MAX_CHARS {
            return Err(format!(
                "an entry in `{field}` is longer than {ENTRY_MAX_CHARS} characters. These are \
                 paths and one-liners; the substance goes in an artefact"
            ));
        }
    }

    if count > LIST_MAX {
        return Err(format!(
            "`{field}` has {count} entries and {LIST_MAX} is the limit — a return is a status, \
             and a list this long is the work rather than a report of it"
        ));
    }
    Ok(())
}

/// The accepted return, in the shape `COS.md` writes it.
///
/// Empty lists print as `—`, so "nothing" differs from "missing".
pub(crate) fn render(report: &Report, summary: &str) -> String {
    let mut out = String::new();

    let _ = writeln!(out, "status: {}", report.status.as_str());
    let _ = writeln!(out, "summary:");
    for line in summary.lines() {
        let _ = writeln!(out, "  {}", line.trim());
    }
    list(
        &mut out,
        "artefacts",
        report.artefacts.iter().map(|a| a.shown.as_str()),
    );
    list(
        &mut out,
        "evidence",
        report.evidence.iter().map(String::as_str),
    );
    list(
        &mut out,
        "open_questions",
        report.open_questions.iter().map(String::as_str),
    );

    let owner = report.next_owner.trim();
    let _ = writeln!(
        out,
        "next_owner: {}",
        if owner.is_empty() { "—" } else { owner }
    );

    out
}

/// One `key:` and its entries, or `—` when it has none.
fn list<'a>(out: &mut String, field: &str, values: impl Iterator<Item = &'a str>) {
    let mut any = false;
    let mut body = String::new();

    for value in values {
        any = true;
        let _ = writeln!(body, "  - {}", value.trim());
    }

    if any {
        let _ = writeln!(out, "{field}:");
        out.push_str(&body);
    } else {
        let _ = writeln!(out, "{field}: —");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    fn brief() -> Brief {
        Brief {
            goal: "Triage what came in this morning".to_owned(),
            owner: "Triager".to_owned(),
            priority: Priority::Normal,
            inputs: vec![".aegis/briefs/from-a-client.md".to_owned()],
            constraints: vec!["do not reply to anyone".to_owned()],
            definition_of_done: ".aegis/status/STATUS.md names the item and its owner".to_owned(),
            approval_needed: "the write to STATUS.md".to_owned(),
            return_format: ReturnFormat::Status,
        }
    }

    #[test]
    fn a_brief_is_accepted_and_rendered_in_the_documented_shape() {
        let rendered = check_brief(&brief()).expect("accepted");

        for key in [
            "goal:",
            "owner:",
            "priority:",
            "inputs:",
            "constraints:",
            "definition_of_done:",
            "approval_needed:",
            "return_format:",
        ] {
            assert!(rendered.contains(key), "missing `{key}` in\n{rendered}");
        }
        assert!(
            rendered.contains(".aegis/briefs/from-a-client.md"),
            "{rendered}"
        );
        assert!(rendered.contains("return_format: status"), "{rendered}");
    }

    /// The rule the whole shape exists for: inputs are paths and links, never a
    /// pasted thread (`COS.md` *Handoff*).
    #[test]
    fn an_input_that_is_pasted_text_is_refused_and_told_what_to_do_instead() {
        let mut pasted = brief();
        pasted.inputs = vec![
            "Here is the thread:\n> can you look at this\n> sure, tomorrow\nso do that".to_owned(),
        ];

        let err = check_brief(&pasted).expect_err("refused");
        assert!(err.contains("inputs"), "{err}");
        assert!(
            err.contains("fs_write"),
            "it says what to do instead: {err}"
        );
    }

    #[test]
    fn an_input_longer_than_a_path_is_refused() {
        let mut wordy = brief();
        wordy.inputs = vec!["x".repeat(BRIEF_ENTRY_MAX_CHARS + 1)];

        let err = check_brief(&wordy).expect_err("refused");
        assert!(err.contains(&BRIEF_ENTRY_MAX_CHARS.to_string()), "{err}");
    }

    #[test]
    fn a_brief_with_no_definition_of_done_is_refused() {
        let mut vague = brief();
        vague.definition_of_done = "  ".to_owned();

        let err = check_brief(&vague).expect_err("refused");
        assert!(err.contains("definition_of_done"), "{err}");
    }

    #[test]
    fn a_brief_with_no_goal_or_no_owner_is_refused() {
        let mut goalless = brief();
        goalless.goal = String::new();
        assert!(check_brief(&goalless).is_err());

        let mut ownerless = brief();
        ownerless.owner = "   ".to_owned();
        let err = check_brief(&ownerless).expect_err("refused");
        assert!(err.contains("owner"), "{err}");
    }

    #[test]
    fn a_goal_that_is_a_specification_is_refused_as_two_briefs() {
        let mut long = brief();
        long.goal = "x".repeat(GOAL_MAX_CHARS + 1);

        let err = check_brief(&long).expect_err("refused");
        assert!(err.contains("two"), "{err}");
    }

    #[test]
    fn a_list_longer_than_the_cap_is_refused_on_the_way_out_too() {
        let mut many = brief();
        many.constraints = (0..=BRIEF_LIST_MAX).map(|n| format!("rule {n}")).collect();

        let err = check_brief(&many).expect_err("refused");
        assert!(err.contains("constraints"), "{err}");
    }

    /// Empty lists and an absent `approval_needed` are printed rather than
    /// dropped, for the reason a report's are: a reader can tell "none" from
    /// "not part of this shape".
    #[test]
    fn an_empty_field_is_still_named() {
        let mut bare = brief();
        bare.constraints.clear();
        bare.approval_needed = String::new();

        let rendered = check_brief(&bare).expect("accepted");
        assert!(rendered.contains("constraints: —"), "{rendered}");
        assert!(rendered.contains("approval_needed: —"), "{rendered}");
    }

    /// Every format states that a `done` needs an artefact or evidence (live
    /// specialists kept missing it).
    #[test]
    fn every_return_format_says_what_a_done_has_to_point_at() {
        for format in [
            ReturnFormat::Status,
            ReturnFormat::Artefact,
            ReturnFormat::Question,
        ] {
            let said = format.expectation();
            assert!(
                said.contains("artefacts") || said.contains("evidence"),
                "{}: {said}",
                format.as_str()
            );
            assert!(said.contains("refused"), "{}: {said}", format.as_str());
        }
    }

    /// A run is told what a complete return looks like, which is the only thing
    /// `return_format` does.
    #[test]
    fn each_return_format_says_something_different_to_the_owner() {
        let said: Vec<&str> = [
            ReturnFormat::Status,
            ReturnFormat::Artefact,
            ReturnFormat::Question,
        ]
        .iter()
        .map(|format| format.expectation())
        .collect();

        assert_eq!(said.len(), 3);
        assert!(said[1].contains("file"), "{}", said[1]);
        assert!(
            said.iter().collect::<std::collections::HashSet<_>>().len() == 3,
            "three formats, three things to say"
        );
    }

    fn report(status: Status) -> Report {
        Report {
            status,
            summary: "Triaged one brief.".to_owned(),
            artefacts: Vec::new(),
            evidence: vec!["read .aegis/briefs/intake.md".to_owned()],
            open_questions: Vec::new(),
            next_owner: String::new(),
        }
    }

    #[test]
    fn a_done_with_evidence_is_accepted_and_rendered_in_the_documented_shape() {
        let rendered = check(&report(Status::Done)).expect("accepted");

        for key in [
            "status:",
            "summary:",
            "artefacts:",
            "evidence:",
            "open_questions:",
            "next_owner:",
        ] {
            assert!(rendered.contains(key), "missing `{key}` in\n{rendered}");
        }
        assert!(rendered.contains("status: done"), "{rendered}");
    }

    /// The rule with the most value in it: a model that says it wrote a file
    /// and did not is the failure this catches, and it catches it by looking.
    #[test]
    fn a_done_naming_an_artefact_that_is_not_there_is_refused() {
        let dir = TempDir::new().expect("temp dir");

        let mut done = report(Status::Done);
        done.artefacts = vec![Artefact {
            shown: ".aegis/artefacts/triage.md".to_owned(),
            path: dir.path().join(".aegis/artefacts/triage.md"),
        }];

        let err = check(&done).expect_err("refused");
        assert!(err.contains(".aegis/artefacts/triage.md"), "{err}");
        assert!(err.contains("blocked"), "it says what to do instead: {err}");
    }

    #[test]
    fn a_done_naming_an_artefact_that_is_there_is_accepted() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("triage.md");
        std::fs::write(&path, "produced").expect("artefact");

        let mut done = report(Status::Done);
        done.evidence.clear();
        done.artefacts = vec![Artefact {
            shown: ".aegis/artefacts/triage.md".to_owned(),
            path,
        }];

        let rendered = check(&done).expect("accepted");
        assert!(
            rendered.contains(".aegis/artefacts/triage.md"),
            "{rendered}"
        );
    }

    #[test]
    fn a_done_pointing_at_nothing_is_refused() {
        let mut done = report(Status::Done);
        done.evidence.clear();

        let err = check(&done).expect_err("refused");
        assert!(err.contains("evidence"), "{err}");
    }

    #[test]
    fn a_blocked_with_no_question_is_refused() {
        for status in [Status::Blocked, Status::NeedsYou] {
            let err = check(&report(status)).expect_err("refused");
            assert!(err.contains("open_questions"), "{err}");
            assert!(err.contains(status.as_str()), "{err}");
        }
    }

    #[test]
    fn a_blocked_with_a_question_is_accepted() {
        let mut blocked = report(Status::Blocked);
        blocked.open_questions = vec!["where is the brief?".to_owned()];

        let rendered = check(&blocked).expect("accepted");
        assert!(rendered.contains("status: blocked"), "{rendered}");
        assert!(rendered.contains("where is the brief?"), "{rendered}");
    }

    #[test]
    fn a_summary_is_five_lines() {
        let mut long = report(Status::Done);
        long.summary = (0..SUMMARY_MAX_LINES + 1)
            .map(|n| format!("line {n}\n"))
            .collect();

        let err = check(&long).expect_err("refused");
        assert!(err.contains(&SUMMARY_MAX_LINES.to_string()), "{err}");
    }

    #[test]
    fn a_return_with_no_summary_is_refused() {
        let mut empty = report(Status::Done);
        empty.summary = "   ".to_owned();

        assert!(check(&empty).is_err());
    }

    #[test]
    fn a_list_longer_than_the_cap_is_refused() {
        let mut noisy = report(Status::Done);
        noisy.evidence = (0..=LIST_MAX).map(|n| format!("check {n}")).collect();

        let err = check(&noisy).expect_err("refused");
        assert!(err.contains("evidence"), "{err}");
    }

    /// Empty lists are printed rather than dropped: a reader can then tell
    /// "this run produced nothing" from "this field is not part of the shape".
    #[test]
    fn an_empty_list_is_still_named() {
        let rendered = check(&report(Status::Done)).expect("accepted");
        assert!(rendered.contains("artefacts: —"), "{rendered}");
    }
}
