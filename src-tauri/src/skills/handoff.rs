//! What a skill run returns, and what makes a return acceptable.
//!
//! `COS.md` *Handoff* fixes the shape every delegation comes back in:
//!
//! ```text
//! status: done | blocked | needs_you
//! summary:             # five lines max
//! artefacts:           # paths
//! evidence:            # tests, screenshot, diff
//! open_questions:
//! next_owner:
//! ```
//!
//! Phase 13 needs the *return* half of it, because a skill declares "what to
//! return (strict format — a handoff result)" and PLAN 7.3 says the runner
//! validates against that declaration. The other half — the brief that goes
//! out — is the handoff bus, and that is Phase 15. This module is deliberately
//! only the return: inventing the outbound object now would be building the
//! bus two phases early, and the shape it would be built to is the one written
//! above, not one derived from what a single skill happened to need.
//!
//! ## What "validate" can honestly mean here
//!
//! A runner cannot judge whether work was done well; that is the human's job,
//! or a verifier skill's (PLAN 7.6, *Verifier is a skill*). What it can do is
//! refuse the returns that are structurally not answers, and there are three:
//!
//! * **`done` pointing at an artefact that is not on disk.** The commonest
//!   failure of a model asked to produce a file is to describe having produced
//!   one. Checking the path costs a `metadata` call and turns that from a
//!   plausible paragraph into a refusal the model can act on.
//! * **`done` pointing at nothing at all.** Fan-in is cheap because every
//!   return names what came of it; a `done` with neither an artefact nor a
//!   piece of evidence is a claim, and Phase 17 could not replay it.
//! * **`blocked` or `needs_you` with no question.** Escalation with nothing to
//!   answer is a dead end for whoever it lands on.
//!
//! Everything else is a cap, and the caps are `COS.md`'s own — a summary is
//! five lines, not a transcript.

use std::fmt::Write as _;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// How a skill run ended (`COS.md` *Handoff*).
///
/// Three states and no fourth. "Partly done" is `needs_you` with the rest in
/// `open_questions`; a runner that offered a fourth would be offering a place
/// to put work nobody then picks up.
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

/// Most characters a summary may have.
///
/// The line cap alone would let one line be a novel. This is the other half of
/// the same rule: the CoS aggregates status, not histories.
pub const SUMMARY_MAX_CHARS: usize = 800;

/// Most entries in any one of the lists.
pub const LIST_MAX: usize = 12;

/// Most characters in one list entry.
pub const ENTRY_MAX_CHARS: usize = 240;

/// Longest `next_owner`.
const OWNER_MAX_CHARS: usize = 64;

/// One artefact the run produced.
///
/// Two forms of the same path: the one the model wrote, which is what a
/// message quotes back, and the resolved one, which is what gets checked. They
/// are kept together because reporting the resolved absolute path in a refusal
/// would answer a question the model did not ask — it named a relative path,
/// and the refusal should be about that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artefact {
    /// The path as the model wrote it.
    pub shown: String,
    /// The same path, resolved and contained by policy.
    pub path: PathBuf,
}

/// A return as the model wrote it, before policy has resolved anything.
///
/// The un-contained half of [`Report`]: `artefacts` are still the strings that
/// arrived. It exists because the resolution belongs to policy — a tool never
/// sees the path the model sent (see [`policy`](crate::policy)) — and a single
/// type carrying both forms would let a caller read whichever one suited it.
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
/// `Ok` carries the normalized report as text: that is what goes back to the
/// model as the tool's content and what a later fan-in reads, so a returned
/// object always looks the same however it was typed. `Err` is a message
/// written for the model — it says which rule was broken and what a return
/// that passes looks like, because the model is the one that has to produce
/// the next attempt.
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
/// Rendered rather than echoed as JSON: this text is read by the model on the
/// next round, and by a person in the transcript, and the `COS.md` block is
/// the form both of them already know. Empty lists are printed as `—` rather
/// than dropped, so a reader can tell "nothing" from "not part of this shape".
fn render(report: &Report, summary: &str) -> String {
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

    fn report(status: Status) -> Report {
        Report {
            status,
            summary: "Triaged one brief.".to_owned(),
            artefacts: Vec::new(),
            evidence: vec!["read briefs/intake.md".to_owned()],
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
            shown: "artefacts/triage.md".to_owned(),
            path: dir.path().join("artefacts/triage.md"),
        }];

        let err = check(&done).expect_err("refused");
        assert!(err.contains("artefacts/triage.md"), "{err}");
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
            shown: "artefacts/triage.md".to_owned(),
            path,
        }];

        let rendered = check(&done).expect("accepted");
        assert!(rendered.contains("artefacts/triage.md"), "{rendered}");
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
