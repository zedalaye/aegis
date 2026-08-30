//! `skill_run` and `skill_return` (PLAN 7.3, Phase 13).
//!
//! A skill is not a tool — `COS.md` is explicit that a tool is a verb on the
//! machine and a skill *sequences* tools toward a done criterion. These two
//! are the verbs: **load this runbook** and **record what came of it**. Asking
//! for a runbook is a file read; recording a result is a validation. Neither
//! is the skill.
//!
//! They are ordinary registry entries for that reason, and because the model
//! has exactly one channel — a tool call — for saying "I am going to follow
//! this procedure now". Putting them anywhere else would mean a second
//! dispatch path beside the one every other call takes, and with it a second
//! place to remember to write an audit line. What it buys, by being here, is
//! that a skill run is gated, audited and cancelled by the machinery that
//! already exists.
//!
//! Three properties are worth reading the code for.
//!
//! **The body arrives here and nowhere else.** Nothing in a system message
//! ever carries a runbook (PLAN 7.6, *catalog in, body on demand*). [`run`]
//! reads the file at the moment it is invoked — so a runbook edited between
//! two turns is the one that runs — and hands it back as the tool's content,
//! for that turn.
//!
//! **A run fails closed rather than halfway.** A runbook declares the tools
//! its steps will call. If the identity does not hold one of them, the run is
//! refused *before* the first step, naming the tool. The alternative is a
//! model four rounds into a procedure discovering it cannot finish, which
//! wastes the rounds and produces a half-done workspace.
//!
//! **A return is checked against the shape, not against taste.** [`ret`]
//! applies `COS.md`'s handoff rules ([`handoff::check`]) — including going and
//! looking at the artefacts a `done` names, which is the one check that
//! catches the commonest failure there is: a model describing a file it never
//! wrote.

use serde_json::{json, Value};

use crate::error::ErrorCode;
use crate::policy::tool;
use crate::skills::{self, handoff, SkillCtx, META_SKILL};

use super::Produced;

/// JSON Schema for `skill_run` arguments.
pub fn run_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": {
                "type": "string",
                "description": "The skill's name, exactly as the catalog spells it.",
            },
        },
        "required": ["name"],
        "additionalProperties": false,
    })
}

/// JSON Schema for `skill_return` arguments (`COS.md` *Handoff*).
pub fn return_schema() -> Value {
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
                "description": "What backs the claim — a test that passed, a diff, a capture.",
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

/// Loads a runbook into this turn.
///
/// The name has already been checked against the identity's skill allow-list
/// by [`policy::decide_call`](crate::policy::decide_call), so everything that
/// can fail here is about the file rather than about rights — except the
/// fail-closed tool check, which is about rights but needs the file to be read
/// before it can be made.
pub(crate) fn run(name: &str, ctx: SkillCtx<'_>) -> Produced {
    let catalog = skills::catalog(ctx.library, ctx.workspace);

    let Some(skill) = skills::find(&catalog, name) else {
        // Granted, but not on disk. Said as two facts rather than one, because
        // they have different fixes: the grant is in Settings, the runbook is
        // in a folder.
        return Produced::failed(
            tool::SKILL_RUN,
            ErrorCode::ToolFailed,
            format!(
                "`{name}` is granted to this identity, but there is no `{}/{name}/{}` in the \
                 library or in this workspace. Nothing here can write one — say so and carry on \
                 without it.",
                skills::LIBRARY_DIR,
                skills::SKILL_FILE
            ),
        );
    };

    if let Some(problem) = &skill.problem {
        return Produced::failed(
            tool::SKILL_RUN,
            ErrorCode::ToolFailed,
            format!("`{name}` is not a runbook this build can follow: {problem}"),
        );
    }

    // Fail closed, and before the first step (PLAN 7.6, *No extra rights*).
    // The refusal is `E_DENIED` because it is one — the runbook is fine, and
    // the identity is not allowed to do what it says.
    if let Some(missing) = ctx.missing(&skill.tools) {
        return Produced::failed(
            tool::SKILL_RUN,
            ErrorCode::Denied,
            format!(
                "`{name}` calls `{missing}`, which this identity does not hold, so the run would \
                 stop partway. A skill grants nothing on its own. Tell the user which tool it \
                 needs."
            ),
        );
    }

    let doc = match skills::load(skill) {
        Ok(doc) => doc,
        Err(problem) => {
            return Produced::failed(
                tool::SKILL_RUN,
                ErrorCode::ToolFailed,
                format!("`{name}` could not be loaded: {problem}"),
            )
        }
    };

    let content = format!(
        "Runbook `{name}`, version {}, from the {}. Follow it in this turn. It grants you \
         nothing: every step is an ordinary tool call and is put to the user exactly as it would \
         be otherwise, and a step you are refused is a `blocked` return rather than a reason to \
         improvise around it. Finish with `skill_return`.\n\n---\n\n{}",
        doc.version,
        match skill.scope {
            skills::SkillScope::Library => "skill library",
            skills::SkillScope::Workspace => "workspace",
        },
        doc.body
    );

    let bytes = doc.body.len() as u64;
    Produced::ok(
        tool::SKILL_RUN,
        format!("loaded {name} (v{})", doc.version),
        content,
        bytes,
        false,
        // `skill` is what the turn loop reads to open the run, and what every
        // audit line written until the return then carries.
        json!({
            META_SKILL: name,
            "version": doc.version,
            "tools": doc.tools,
        }),
    )
    // Named on its own line too, so the opening of a run is on the record even
    // though nothing was running when it was made.
    .in_skill(name)
}

/// Records the result of the runbook that was loaded.
///
/// Refused when nothing is running: a return with no run behind it is a status
/// object about nothing, and accepting it would put a `done` on the audit log
/// for work no skill ever framed.
pub(crate) fn ret(report: &handoff::Report, ctx: SkillCtx<'_>) -> Produced {
    let Some(active) = ctx.active else {
        return Produced::failed(
            tool::SKILL_RETURN,
            ErrorCode::ToolFailed,
            "no skill is running in this turn, so there is nothing to return. `skill_run` opens \
             one, and a run lasts for the turn that opened it."
                .to_owned(),
        );
    };

    match handoff::check(report) {
        Ok(rendered) => {
            let bytes = rendered.len() as u64;
            Produced::ok(
                tool::SKILL_RETURN,
                format!(
                    "{active} — {}{}",
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
                    META_SKILL: active,
                    "status": report.status.as_str(),
                    "artefacts": report.artefacts.len(),
                }),
            )
        }
        // The run stays open. A refused return is one the model is expected to
        // make again, corrected, and closing the span here would take the
        // skill's name off the audit lines of the attempts that follow.
        Err(reason) => Produced::failed(tool::SKILL_RETURN, ErrorCode::ToolFailed, reason),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::TempDir;

    use super::*;
    use crate::skills::{SKILL_FILE, TRIAGE_SEED};

    /// A library holding `inbox.triage`, and a context that may run it.
    fn library() -> TempDir {
        let dir = TempDir::new().expect("temp dir");
        let skill = dir.path().join("inbox.triage");
        fs::create_dir_all(&skill).expect("skill dir");
        fs::write(skill.join(SKILL_FILE), TRIAGE_SEED).expect("runbook");
        dir
    }

    fn ctx<'a>(library: &'a Path, tools: &'a [String], active: Option<&'a str>) -> SkillCtx<'a> {
        SkillCtx {
            library,
            workspace: None,
            tools,
            active,
        }
    }

    fn every_tool() -> Vec<String> {
        crate::tools::names()
            .iter()
            .map(|name| (*name).to_owned())
            .collect()
    }

    #[test]
    fn a_run_hands_back_the_body_and_names_the_run_for_the_audit() {
        let dir = library();
        let held = every_tool();
        let produced = run("inbox.triage", ctx(dir.path(), &held, None));

        assert!(produced.result.ok, "{:?}", produced.result.error);
        assert!(
            produced.result.content.contains("Rewrite the file whole"),
            "the body is what comes back"
        );
        assert!(
            produced.result.content.contains("grants you nothing"),
            "the runner says what a run is not"
        );
        assert_eq!(
            produced.result.meta[META_SKILL].as_str(),
            Some("inbox.triage")
        );
        assert_eq!(produced.skill.as_deref(), Some("inbox.triage"));
    }

    /// PLAN 7.6, *No extra rights*: the run stops before the first step, and
    /// the refusal names the tool so the user knows what to grant.
    #[test]
    fn a_runbook_calling_a_tool_the_identity_lacks_fails_before_the_first_step() {
        let dir = library();
        let held = vec![crate::policy::tool::FS_READ.to_owned()];
        let produced = run("inbox.triage", ctx(dir.path(), &held, None));

        assert!(!produced.result.ok);
        let error = produced.result.error.expect("an error");
        assert_eq!(error.code, ErrorCode::Denied);
        assert!(
            error.message.contains(crate::policy::tool::FS_LIST),
            "{}",
            error.message
        );
        assert!(
            error.message.contains("grants nothing"),
            "{}",
            error.message
        );
    }

    #[test]
    fn a_granted_skill_with_no_runbook_says_where_one_would_go() {
        let dir = TempDir::new().expect("temp dir");
        let held = every_tool();
        let produced = run("inbox.triage", ctx(dir.path(), &held, None));

        assert!(!produced.result.ok);
        let message = produced.result.error.expect("an error").message;
        assert!(message.contains("inbox.triage"), "{message}");
        assert!(message.contains(SKILL_FILE), "{message}");
    }

    #[test]
    fn a_broken_runbook_is_reported_with_what_is_wrong_with_it() {
        let dir = TempDir::new().expect("temp dir");
        let skill = dir.path().join("broken");
        fs::create_dir_all(&skill).expect("skill dir");
        fs::write(skill.join(SKILL_FILE), "no front matter here").expect("runbook");

        let held = every_tool();
        let produced = run("broken", ctx(dir.path(), &held, None));

        assert!(!produced.result.ok);
        assert!(produced
            .result
            .error
            .expect("an error")
            .message
            .contains("---"));
    }

    #[test]
    fn a_return_with_no_run_behind_it_is_refused() {
        let dir = library();
        let held = every_tool();
        let report = handoff::Report {
            status: handoff::Status::Done,
            summary: "did the thing".to_owned(),
            artefacts: Vec::new(),
            evidence: vec!["looked".to_owned()],
            open_questions: Vec::new(),
            next_owner: String::new(),
        };

        let produced = ret(&report, ctx(dir.path(), &held, None));
        assert!(!produced.result.ok);
        assert!(produced
            .result
            .error
            .expect("an error")
            .message
            .contains("skill_run"));
    }

    #[test]
    fn an_accepted_return_carries_the_documented_shape_and_names_the_run() {
        let dir = library();
        let held = every_tool();
        let report = handoff::Report {
            status: handoff::Status::Blocked,
            summary: "briefs/ is empty".to_owned(),
            artefacts: Vec::new(),
            evidence: Vec::new(),
            open_questions: vec!["which item should I triage?".to_owned()],
            next_owner: "human".to_owned(),
        };

        let produced = ret(&report, ctx(dir.path(), &held, Some("inbox.triage")));

        assert!(produced.result.ok, "{:?}", produced.result.error);
        assert!(produced.result.content.contains("status: blocked"));
        assert!(produced
            .result
            .content
            .contains("which item should I triage?"));
        assert_eq!(
            produced.result.meta[META_SKILL].as_str(),
            Some("inbox.triage")
        );
        assert!(
            produced.summary.starts_with("inbox.triage — blocked"),
            "{}",
            produced.summary
        );
    }

    /// A refused return leaves the run open, so the corrected attempt is still
    /// audited as part of the same skill.
    #[test]
    fn a_refused_return_does_not_close_the_run() {
        let dir = library();
        let held = every_tool();
        let report = handoff::Report {
            status: handoff::Status::Done,
            summary: "wrote the triage note".to_owned(),
            artefacts: vec![handoff::Artefact {
                shown: "artefacts/x.md".to_owned(),
                path: dir.path().join("artefacts/x.md"),
            }],
            evidence: Vec::new(),
            open_questions: Vec::new(),
            next_owner: String::new(),
        };

        let produced = ret(&report, ctx(dir.path(), &held, Some("inbox.triage")));
        assert!(!produced.result.ok);

        let mut active = Some("inbox.triage".to_owned());
        skills::track(&mut active, tool::SKILL_RETURN, &produced.result);
        assert_eq!(active.as_deref(), Some("inbox.triage"));
    }
}
