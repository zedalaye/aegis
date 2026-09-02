//! `handoff_delegate` and `handoff_return` (PLAN 7.3, Phase 15).
//!
//! The two verbs of the Chef-de-Cabinet loop: **hand this out** and **here is
//! what came of it**. Like the skill tools beside them they are ordinary
//! registry entries, because the model has exactly one channel for saying "I am
//! routing this now", and a second dispatch path would be a second place to
//! remember the audit line.
//!
//! Four properties are worth reading the code for.
//!
//! **A brief is checked before a person is asked about it.** The whole
//! validation happens in the policy table ([`handoff::check_brief`]), so a
//! delegation that would be refused never reaches an approval dialog. What is
//! left here is what only the runtime knows: whether this turn is allowed to
//! delegate at all.
//!
//! **The wait is bounded and the CoS keeps its turn.** [`delegate`] awaits the
//! bus, which runs every brief in parallel under its own deadline and gives up
//! after two failures ([`bus`]). The tool cannot hang: the worst case is one
//! [`bus::ATTEMPT_TIMEOUT`] plus a review, and the result is a board that says
//! who did not answer.
//!
//! **What comes back is a board, never a transcript.** [`bus::Board::render`]
//! is statuses, artefact paths and open questions. There is no code path from a
//! specialist's messages to the CoS's context, and that is structural rather
//! than careful: the only thing a delegated run can produce is a
//! [`handoff::Report`],
//! and a report has no field wide enough for a conversation (`COS.md`: *the CoS
//! aggregates status, not histories*).
//!
//! **A return is checked against the shape, not against taste.** [`ret`] is
//! [`handoff::check`], the same function `skill_return` uses — including going
//! and looking at the artefacts a `done` names, which catches the commonest
//! failure there is: a model describing a file it never wrote.

use serde_json::{json, Value};

use crate::error::ErrorCode;
use crate::handoff::{self, bus};
use crate::policy::tool;

use super::Produced;

/// Where a delegation is recorded, in [`ToolResult::meta`](super::ToolResult).
///
/// The turn reads it out of the envelope rather than out of the arguments, for
/// the reason [`META_SKILL`](crate::skills::META_SKILL) is read that way: what
/// happened is what the tool decided, and a loop that re-derived it from the
/// call would be a second copy of that decision.
pub const META_HANDOFF: &str = "handoff";

/// What a handoff tool needs from the runtime around it.
///
/// Held by [`ToolCtx`](super::ToolCtx) rather than resolved inside the tool,
/// for the reason the skill library is: who can run a brief, and whether this
/// turn is itself one, are facts about the application and the session, not
/// about what the model asked for. A tool that went looking for them could not
/// be tested without an application.
#[derive(Clone, Copy)]
pub struct HandoffCtx<'a> {
    /// Who carries a brief, when this turn is allowed to hand one out.
    ///
    /// `None` in a delegated run — depth is one (`COS.md` *Roles*) — and in any
    /// turn the application did not give a bus, which is what a test gets.
    pub bus: Option<&'a std::sync::Arc<dyn bus::Runner>>,
    /// The delegated run this turn *is*, when it is one.
    ///
    /// `Some` exactly when a brief opened this turn. It is where a
    /// `handoff_return` lands, and it is what puts the delegation's id on every
    /// audit line the run writes.
    pub open: Option<&'a handoff::Open>,
}

impl HandoffCtx<'_> {
    /// The delegation this call belongs to, for the audit line.
    pub fn id(&self) -> &str {
        self.open.map_or("", handoff::Open::id)
    }
}

/// JSON Schema for `handoff_delegate` arguments (`COS.md` *Handoff*).
pub fn delegate_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "briefs": {
                "type": "array",
                "minItems": 1,
                "maxItems": bus::FAN_OUT_MAX,
                "description": "One per identity you are handing work to. They run at the same \
                                time and each one gets a session of its own.",
                "items": brief_schema(),
            },
            "review": {
                "description": "An optional brief for an identity that checks the others' work \
                                once they are back. It is given their artefacts as inputs and \
                                nothing else — never their sessions.",
                "allOf": [brief_schema()],
            },
        },
        "required": ["briefs"],
        "additionalProperties": false,
    })
}

/// JSON Schema for one brief.
fn brief_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "goal": {
                "type": "string",
                "description": "What is to be achieved, in a line or two.",
            },
            "owner": {
                "type": "string",
                "description": "The identity that will do it, by name. It works under its own \
                                tools and its own memory, not yours.",
            },
            "priority": {
                "type": "string",
                "enum": ["high", "normal", "low"],
                "description": "Defaults to normal.",
            },
            "inputs": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Paths and URLs the work starts from. Never pasted text: if the \
                                owner needs a document, write it with `fs_write` and name the \
                                path here. A multi-line entry is refused.",
            },
            "constraints": {
                "type": "array",
                "items": { "type": "string" },
                "description": "What it must respect, and what it must not do.",
            },
            "definition_of_done": {
                "type": "string",
                "description": "How the owner knows to stop. Required — without it the return is \
                                somebody's opinion that enough has happened.",
            },
            "approval_needed": {
                "type": "string",
                "description": "Which parts of this you expect to stop and ask a human. It \
                                grants nothing either way: every step is gated as usual.",
            },
            "return_format": {
                "type": "string",
                "enum": ["status", "artefact", "question"],
                "description": "What a complete return looks like. Defaults to status.",
            },
        },
        "required": ["goal", "owner", "definition_of_done"],
        "additionalProperties": false,
    })
}

/// JSON Schema for a return — `handoff_return` and `skill_return` alike.
///
/// One schema for both, because it is one object (`COS.md` *Handoff*). Two
/// copies would be two places for the shape to drift.
pub fn report_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "status": {
                "type": "string",
                "enum": ["done", "blocked", "needs_you"],
                "description": "done when the definition of done is met; blocked when a source \
                                or a step was not available; needs_you when it turns on a \
                                decision only the human can make.",
            },
            "summary": {
                "type": "string",
                "description": "What happened, five lines at most.",
            },
            "artefacts": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Paths inside the workspace that this run produced. They are \
                                checked: a `done` naming a file that is not there is refused.",
            },
            "evidence": {
                "type": "array",
                "items": { "type": "string" },
                "description": "What backs the claim — a test that passed, a diff, a capture, or                                 simply the file you read. Required for a `done` that names no                                 artefacts: a done pointing at nothing is refused, because nobody                                 can check it afterwards.",
            },
            "open_questions": {
                "type": "array",
                "items": { "type": "string" },
                "description": "What the next owner has to answer. Required for blocked and \
                                needs_you.",
            },
            "next_owner": {
                "type": "string",
                "description": "Who should pick this up, if anyone.",
            },
        },
        "required": ["status", "summary"],
        "additionalProperties": false,
    })
}

/// Hands the briefs out, waits, and reports the board.
///
/// The briefs were validated by the policy table and approved by the user
/// before anything got here, so the only refusal left is the structural one:
/// this turn has no bus.
pub(crate) async fn delegate(
    plan: &handoff::Plan,
    ctx: HandoffCtx<'_>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Produced {
    let Some(runner) = ctx.bus else {
        // Reachable two ways, and the message covers both: a turn the
        // application gave no bus, and a delegated run replaying a call out of
        // an older transcript. Policy refuses the second before this, so what
        // is usually left is the first.
        return Produced::failed(
            tool::HANDOFF_DELEGATE,
            ErrorCode::ToolFailed,
            "this session cannot hand work out. Do what you can here, or tell the user what you \
             would have delegated and to whom."
                .to_owned(),
        );
    };

    let board = bus::deliver(runner, plan.clone(), cancel).await;
    let rendered = board.render();
    let bytes = rendered.len() as u64;

    Produced::ok(
        tool::HANDOFF_DELEGATE,
        board.headline(),
        rendered,
        bytes,
        false,
        json!({
            META_HANDOFF: board.id,
            "briefs": board.assignments.len(),
            "done": board.done(),
            "blocked": board.blocked(),
            "needs_you": board.needs_you(),
            "review": board.review.as_ref().map(|one| one.outcome.status().as_str()),
        }),
    )
    // Named on the line of the call that started it, as `skill_run` is: nothing
    // was open when it was made, so the opening of a delegation would otherwise
    // be the one event of a delegation that is not on the record.
    .in_handoff(&board.id)
}

/// Records the result of the brief this run was given.
///
/// Refused when no brief opened this turn: a return with nothing behind it is a
/// status object about nothing, and accepting it would put a `done` on the
/// audit log for work nobody asked for.
pub(crate) fn ret(report: &handoff::Report, ctx: HandoffCtx<'_>) -> Produced {
    let Some(open) = ctx.open else {
        return Produced::failed(
            tool::HANDOFF_RETURN,
            ErrorCode::ToolFailed,
            "no brief opened this turn, so there is nothing to return. This tool closes a \
             delegated run; in an ordinary session, just answer."
                .to_owned(),
        );
    };

    match handoff::check(report) {
        Ok(rendered) => {
            open.close(report.clone());

            let bytes = rendered.len() as u64;
            Produced::ok(
                tool::HANDOFF_RETURN,
                format!(
                    "returned {}{}",
                    report.status.as_str(),
                    match report.artefacts.len() {
                        0 => String::new(),
                        1 => ", 1 artefact".to_owned(),
                        n => format!(", {n} artefacts"),
                    }
                ),
                rendered,
                bytes,
                false,
                json!({
                    META_HANDOFF: open.id(),
                    "status": report.status.as_str(),
                    "artefacts": report.artefacts.len(),
                }),
            )
        }
        // The run stays open, so the corrected attempt is still part of it —
        // and so the turn does not end on a return that was refused.
        Err(reason) => Produced::failed(tool::HANDOFF_RETURN, ErrorCode::ToolFailed, reason),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handoff::{Brief, Priority, Report, ReturnFormat, Status};

    fn ctx<'a>(open: Option<&'a handoff::Open>) -> HandoffCtx<'a> {
        HandoffCtx { bus: None, open }
    }

    fn report(status: Status) -> Report {
        Report {
            status,
            summary: "read the brief and wrote the note".to_owned(),
            artefacts: Vec::new(),
            evidence: vec![".aegis/briefs/intake.md".to_owned()],
            open_questions: match status {
                Status::Done => Vec::new(),
                _ => vec!["which inbox?".to_owned()],
            },
            next_owner: String::new(),
        }
    }

    #[tokio::test]
    async fn a_delegation_from_a_turn_with_no_bus_is_refused_rather_than_silently_dropped() {
        let plan = handoff::Plan {
            briefs: vec![Brief {
                goal: "draft it".to_owned(),
                owner: "Scribe".to_owned(),
                priority: Priority::Normal,
                inputs: Vec::new(),
                constraints: Vec::new(),
                definition_of_done: "the file is there".to_owned(),
                approval_needed: String::new(),
                return_format: ReturnFormat::Artefact,
            }],
            review: None,
        };

        let produced = delegate(
            &plan,
            ctx(None),
            &tokio_util::sync::CancellationToken::new(),
        )
        .await;

        assert!(!produced.result.ok);
        assert!(produced
            .result
            .error
            .expect("an error")
            .message
            .contains("cannot hand work out"));
    }

    #[test]
    fn a_return_with_no_brief_behind_it_is_refused() {
        let produced = ret(&report(Status::Done), ctx(None));

        assert!(!produced.result.ok);
        assert!(produced
            .result
            .error
            .expect("an error")
            .message
            .contains("no brief opened this turn"));
    }

    /// The accepted return lands in the cell, which is what the runner reads
    /// and what stops the turn taking another round.
    #[test]
    fn an_accepted_return_closes_the_run_and_names_the_delegation() {
        let open = handoff::Open::new("d-1");
        let produced = ret(&report(Status::Done), ctx(Some(&open)));

        assert!(produced.result.ok, "{:?}", produced.result.error);
        assert!(open.closed());
        assert_eq!(open.take().expect("a report").status, Status::Done);
        assert_eq!(produced.result.meta[META_HANDOFF].as_str(), Some("d-1"));
        assert!(produced.result.content.contains("status: done"));
    }

    /// A refused return leaves the run open, so the turn gets another round to
    /// correct it rather than ending with nothing.
    #[test]
    fn a_refused_return_leaves_the_run_open() {
        let open = handoff::Open::new("d-1");
        let mut blocked = report(Status::Blocked);
        blocked.open_questions.clear();

        let produced = ret(&blocked, ctx(Some(&open)));

        assert!(!produced.result.ok);
        assert!(!open.closed(), "the run is still waiting for an answer");
    }
}
