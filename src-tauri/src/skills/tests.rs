use super::*;

use tempfile::TempDir;

use crate::policy::tool;
use crate::store::DEFAULT_AGENT_ID;

/// A library with `names` in it, each holding a runbook that parses.
fn library(names: &[&str]) -> TempDir {
    let dir = TempDir::new().expect("temp dir");
    for name in names {
        write_skill(dir.path(), name, TRIAGE_SEED);
    }
    dir
}

fn write_skill(root: &Path, name: &str, text: &str) {
    let dir = root.join(name);
    fs::create_dir_all(&dir).expect("skill dir");
    fs::write(dir.join(SKILL_FILE), text).expect("runbook");
}

fn agent_with(skills: &[&str]) -> Agent {
    let mut agent = Agent::builtin();
    agent.id = "a1".to_owned();
    agent.name = "Triager".to_owned();
    agent.builtin = false;
    agent.skills = skills.iter().map(|name| (*name).to_owned()).collect();
    agent
}

#[test]
fn a_directory_holding_a_runbook_is_a_skill() {
    let dir = library(&["inbox.triage"]);
    let found = catalog(dir.path(), None);

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].name, "inbox.triage");
    assert_eq!(found[0].scope, SkillScope::Library);
    assert_eq!(found[0].version, "1");
    assert!(found[0].runnable(), "{:?}", found[0].problem);
    assert_eq!(
        found[0].tools,
        vec![tool::FS_LIST, tool::FS_READ, tool::FS_WRITE]
    );
}

/// Writes `.aegis/skills/<name>/<file>` under a workspace root.
fn propose(root: &Path, name: &str, file: &str, text: &str) {
    let dir = workspace_dir(root).join(name);
    fs::create_dir_all(&dir).expect("skill dir");
    fs::write(dir.join(file), text).expect("file");
}

/// PLAN 7.13, *What the catalog sees*: only `SKILL.md`. A proposal is
/// listed where a person can apply it and nowhere a model could run it.
#[test]
fn a_proposal_is_listed_and_never_reaches_the_catalog() {
    let root = TempDir::new().expect("workspace");
    let empty = TempDir::new().expect("library");
    propose(root.path(), "inbox.triage", PROPOSAL_FILE, TRIAGE_SEED);

    assert!(catalog(empty.path(), Some(root.path())).is_empty());
    assert!(is_proposed(root.path(), "inbox.triage"));

    let listed = proposals(root.path());
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "inbox.triage");
    assert_eq!(listed[0].state, ProposalState::Pending);
    assert_eq!(listed[0].version, "1");
    assert_eq!(listed[0].problem, None);
    assert!(
        listed[0].target.ends_with(SKILL_FILE),
        "{}",
        listed[0].target
    );
}

#[test]
fn a_proposal_says_whether_it_was_applied_or_would_replace_a_runbook() {
    let root = TempDir::new().expect("workspace");
    propose(root.path(), "applied", PROPOSAL_FILE, TRIAGE_SEED);
    propose(root.path(), "applied", SKILL_FILE, TRIAGE_SEED);
    propose(root.path(), "occupied", PROPOSAL_FILE, TRIAGE_SEED);
    propose(root.path(), "occupied", SKILL_FILE, REVIEW_SEED);

    let states: Vec<(String, ProposalState)> = proposals(root.path())
        .into_iter()
        .map(|proposal| (proposal.name, proposal.state))
        .collect();
    assert_eq!(
        states,
        vec![
            ("applied".to_owned(), ProposalState::Applied),
            ("occupied".to_owned(), ProposalState::Occupied),
        ]
    );
}

#[test]
fn a_proposal_that_will_not_parse_is_listed_with_the_reason() {
    let root = TempDir::new().expect("workspace");
    propose(root.path(), "half", PROPOSAL_FILE, "no front matter here");

    let listed = proposals(root.path());
    assert_eq!(listed.len(), 1);
    assert!(listed[0].problem.is_some());
}

/// An apply is recognised by what the write is — the proposal, copied — and
/// every other write of a `SKILL.md` is the handwritten path, left alone.
#[test]
fn an_apply_is_a_copy_of_the_proposal_and_nothing_else_is_one() {
    let root = TempDir::new().expect("workspace");
    propose(root.path(), "inbox.triage", PROPOSAL_FILE, TRIAGE_SEED);
    let target = Path::new(".aegis/skills/inbox.triage/SKILL.md");

    assert_eq!(
        apply_of(root.path(), target, TRIAGE_SEED),
        Some(Ok("inbox.triage".to_owned()))
    );
    // Spelled the way a case-folding filesystem would still reach.
    assert_eq!(
        apply_of(
            root.path(),
            Path::new(".Aegis/Skills/inbox.triage/skill.md"),
            TRIAGE_SEED
        ),
        Some(Ok("inbox.triage".to_owned()))
    );

    // A handwritten runbook, and the proposal written somewhere else.
    assert_eq!(apply_of(root.path(), target, REVIEW_SEED), None);
    assert_eq!(
        apply_of(root.path(), Path::new("notes/SKILL.md"), TRIAGE_SEED),
        None
    );
    assert_eq!(
        apply_of(
            root.path(),
            Path::new(".aegis/skills/other/SKILL.md"),
            TRIAGE_SEED
        ),
        None
    );
}

#[test]
fn an_apply_is_refused_over_a_runbook_or_from_a_broken_proposal() {
    let root = TempDir::new().expect("workspace");

    propose(root.path(), "broken", PROPOSAL_FILE, "no front matter here");
    let refused = apply_of(
        root.path(),
        Path::new(".aegis/skills/broken/SKILL.md"),
        "no front matter here",
    );
    let reason = refused.expect("an apply").expect_err("refused");
    assert!(reason.contains("never applied"), "{reason}");

    propose(root.path(), "handwritten", PROPOSAL_FILE, TRIAGE_SEED);
    propose(root.path(), "handwritten", SKILL_FILE, REVIEW_SEED);
    let refused = apply_of(
        root.path(),
        Path::new(".aegis/skills/handwritten/SKILL.md"),
        TRIAGE_SEED,
    );
    let reason = refused.expect("an apply").expect_err("refused");
    assert!(reason.contains("never replaces"), "{reason}");
    assert_eq!(
        fs::read_to_string(
            workspace_dir(root.path())
                .join("handwritten")
                .join(SKILL_FILE)
        )
        .expect("still there"),
        REVIEW_SEED
    );
}

/// The seeded runbooks are the format's own documentation. If one stops
/// parsing, every example a user copies from it is wrong.
#[test]
fn every_seeded_runbook_parses() {
    for (name, text) in SEEDED.iter().chain([&("inbox.triage", TRIAGE_SEED)]) {
        doc::parse(text).unwrap_or_else(|err| panic!("`{name}` does not parse: {err}"));
    }
}

/// Every runbook of PLAN 7.3's Phase 19, in the order the packs landed.
const PACKS: [&str; 18] = [
    REVIEW_DIFF_SKILL,
    DEPLOY_SKILL,
    ALERT_SKILL,
    MAIL_SKILL,
    THREAD_SKILL,
    REPLY_SKILL,
    WATCH_SWEEP_SKILL,
    WATCH_DIGEST_SKILL,
    WATCH_IMPACT_SKILL,
    BUDGET_POSITION_SKILL,
    BUDGET_RUNWAY_SKILL,
    BUDGET_ALERT_SKILL,
    SOCIAL_SCAN_SKILL,
    SOCIAL_REPLY_SKILL,
    SOCIAL_POST_SKILL,
    WISH_LIST_SKILL,
    REVENUE_THESIS_SKILL,
    REVENUE_PIPELINE_SKILL,
];

/// The parsed runbook a seeded name ships with.
fn seeded(name: &str) -> doc::SkillDoc {
    let (_, text) = SEEDED
        .iter()
        .find(|(seeded, _)| *seeded == name)
        .unwrap_or_else(|| panic!("`{name}` is seeded"));
    doc::parse(text).unwrap_or_else(|err| panic!("`{name}`: {err}"))
}

/// A seeded pack runbook declares no connector tool, or it would fail
/// closed on a fresh install.
#[test]
fn a_domain_pack_calls_only_tools_this_build_has() {
    for name in PACKS {
        let doc = seeded(name);

        assert!(!doc.tools.is_empty(), "`{name}` declares what it calls");
        for tool in &doc.tools {
            assert!(
                crate::tools::spec(tool).is_some(),
                "`{name}` declares `{tool}`, which needs something installed"
            );
        }
    }
}

/// Each pack runbook's *When to use it* opening fits
/// [`doc::SUMMARY_MAX_CHARS`], so its catalog line is not cut off.
#[test]
fn a_domain_pack_says_when_to_run_it_without_the_line_being_cut_off() {
    for name in PACKS {
        let summary = seeded(name).summary;
        assert!(
            !summary.ends_with('…'),
            "`{name}` opens with {} characters and the catalog keeps {}: {summary}",
            summary.chars().count(),
            doc::SUMMARY_MAX_CHARS
        );
    }
}

/// Seeded packs reach no model until an identity is granted them.
#[test]
fn the_delivery_pack_reaches_a_specialist_and_nobody_else() {
    let dir = TempDir::new().expect("temp dir");
    seed(dir.path());
    let catalog = catalog(dir.path(), None);

    let specialist = agent_with(&[REVIEW_DIFF_SKILL, DEPLOY_SKILL, ALERT_SKILL]);
    let held: Vec<&str> = granted(&catalog, &specialist)
        .iter()
        .map(|skill| skill.name.as_str())
        .collect();
    assert_eq!(held, [ALERT_SKILL, DEPLOY_SKILL, REVIEW_DIFF_SKILL]);

    assert!(
        granted(&catalog, &Agent::builtin()).is_empty(),
        "installing a pack grants nothing"
    );
}

/// Intake (pack 2) declares no command: its identity reads text strangers
/// wrote and must not need a shell.
#[test]
fn the_intake_pack_holds_no_tool_that_runs_a_command() {
    for name in [MAIL_SKILL, THREAD_SKILL, REPLY_SKILL] {
        assert!(
            !seeded(name)
                .tools
                .iter()
                .any(|held| held == tool::SHELL_EXEC),
            "`{name}` declares `{}`; intake is granted to an identity that has none",
            tool::SHELL_EXEC
        );
    }
}

/// The watch pack (pack 3) passes the real routine door
/// ([`schedule::check`](crate::schedule::check)) and declares no command: an
/// unattended run must not have an outbound channel.
#[test]
fn the_watch_pack_can_be_put_on_a_clock() {
    use crate::policy::Grant;
    use crate::store::routines::{RoutineDraft, Schedule};

    let dir = TempDir::new().expect("temp dir");
    seed(dir.path());
    let catalog = catalog(dir.path(), None);

    let mut watcher = agent_with(&[WATCH_SWEEP_SKILL, WATCH_DIGEST_SKILL, WATCH_IMPACT_SKILL]);
    watcher.name = "Watcher".to_owned();
    // Exactly what the three declare: no shell, no screen.
    watcher.tools = vec![
        tool::FS_LIST.to_owned(),
        tool::FS_READ.to_owned(),
        tool::FS_WRITE.to_owned(),
        tool::SKILL_RUN.to_owned(),
        tool::SKILL_RETURN.to_owned(),
    ];

    let draft = |name: &str, grants: Vec<Grant>| RoutineDraft {
        name: "Morning watch".to_owned(),
        project_id: "p1".to_owned(),
        agent_id: watcher.id.clone(),
        skill: name.to_owned(),
        schedule: Schedule::DailyAt { hour: 7, minute: 0 },
        grants,
        runs_per_day: 4,
    };

    for name in [WATCH_SWEEP_SKILL, WATCH_DIGEST_SKILL, WATCH_IMPACT_SKILL] {
        let skill = find(&catalog, name).expect("seeded");
        // The only standing approval needed: workspace writes.
        crate::schedule::check(
            &draft(name, vec![Grant::FsWrite]),
            &watcher,
            Some(skill),
            true,
        )
        .unwrap_or_else(|err| panic!("`{name}` cannot be scheduled: {err}"));
    }

    // No clock may amend the constitution: the door refuses that grant.
    let impact = find(&catalog, WATCH_IMPACT_SKILL).expect("seeded");
    assert!(
        crate::schedule::check(
            &draft(WATCH_IMPACT_SKILL, vec![Grant::WorldAmend]),
            &watcher,
            Some(impact),
            true,
        )
        .is_err(),
        "an écart is not something a routine signs for"
    );
}

/// Budget (pack 4) declares only `fs_read`, `fs_list` and `fs_write`: no
/// shell that could become a broker client (PLAN 7.3, *not a broker*).
#[test]
fn the_budget_pack_could_not_reach_a_broker_if_it_tried() {
    let allowed = [tool::FS_LIST, tool::FS_READ, tool::FS_WRITE];

    for name in [
        BUDGET_POSITION_SKILL,
        BUDGET_RUNWAY_SKILL,
        BUDGET_ALERT_SKILL,
    ] {
        for declared in seeded(name).tools {
            assert!(
                allowed.contains(&declared.as_str()),
                "`{name}` declares `{declared}`; surveillance reads files and writes one back"
            );
        }
    }
}

/// Every runbook whose output a person sends or publishes ends by naming
/// [`REVIEW_SKILL`] (PLAN 7.6, *Verifier is a skill*).
#[test]
fn a_draft_somebody_else_sends_names_the_runbook_that_checks_it() {
    for name in [
        ALERT_SKILL,
        REPLY_SKILL,
        SOCIAL_REPLY_SKILL,
        SOCIAL_POST_SKILL,
    ] {
        assert!(
            seeded(name).body.contains(REVIEW_SKILL),
            "`{name}` ends in something a person sends and never names `{REVIEW_SKILL}`"
        );
    }
}

/// Social (pack 5) declares only file tools, so no later edit quietly adds a
/// way to publish (PLAN 7.4).
#[test]
fn the_social_pack_holds_nothing_that_could_publish() {
    let allowed = [tool::FS_LIST, tool::FS_READ, tool::FS_WRITE];

    for name in [SOCIAL_SCAN_SKILL, SOCIAL_REPLY_SKILL, SOCIAL_POST_SKILL] {
        for declared in seeded(name).tools {
            assert!(
                allowed.contains(&declared.as_str()),
                "`{name}` declares `{declared}`; a draft is a file until a person posts it"
            );
        }
    }
}

/// Revenue and wish list (pack 6) keep goals in files, never `memory_write`
/// (PLAN 7.4: memories are per-identity, capped and deleted with it), with
/// the budget pack's file-only perimeter.
#[test]
fn a_goal_is_a_file_and_not_something_an_identity_remembers() {
    let allowed = [tool::FS_LIST, tool::FS_READ, tool::FS_WRITE];

    for name in [
        WISH_LIST_SKILL,
        REVENUE_THESIS_SKILL,
        REVENUE_PIPELINE_SKILL,
    ] {
        for declared in seeded(name).tools {
            assert!(
                allowed.contains(&declared.as_str()),
                "`{name}` declares `{declared}`; a goal is a file somebody can open and delete"
            );
        }
    }
}

/// The whole seeded catalog stays within the per-line bound times its size;
/// [`the_catalog_block_carries_no_step_of_any_runbook`] bounds one line.
///
/// [`the_catalog_block_carries_no_step_of_any_runbook`]: self#tests
#[test]
fn a_library_holding_every_pack_still_costs_a_bounded_block() {
    let dir = TempDir::new().expect("temp dir");
    seed(dir.path());
    let catalog = catalog(dir.path(), None);

    let mut everything = Agent::builtin();
    everything.id = "a1".to_owned();
    everything.builtin = false;
    everything.skills = SEEDED.iter().map(|(name, _)| (*name).to_owned()).collect();

    let held = granted(&catalog, &everything);
    assert_eq!(held.len(), SEEDED.len(), "an identity granted all of them");

    let block = prompt_block(&held).expect("a block");
    let bound = SEEDED.len() * (doc::SUMMARY_MAX_CHARS + NAME_MAX_CHARS + 64) + 512;
    assert!(
        block.len() <= bound,
        "the whole library costs {} characters of every request, over the {bound} its own \
         per-line cap allows",
        block.len()
    );

    // And the property the cap exists to protect: it is still a catalog.
    assert!(
        !block.contains("## Steps"),
        "a runbook's steps reached the system message:\n{block}"
    );
}

#[test]
fn a_folder_that_is_not_a_skill_is_not_one() {
    let dir = TempDir::new().expect("temp dir");
    fs::create_dir_all(dir.path().join("notes")).expect("a plain folder");
    fs::write(dir.path().join("README.md"), "hello").expect("a plain file");
    write_skill(dir.path(), "Not A Name", TRIAGE_SEED);

    assert!(catalog(dir.path(), None).is_empty());
}

/// A runbook that will not parse stays in the catalog carrying its
/// refusal. Vanishing would leave the author with nothing to fix.
#[test]
fn a_broken_runbook_is_listed_with_its_problem_and_never_offered() {
    let dir = TempDir::new().expect("temp dir");
    write_skill(dir.path(), "broken", "# no front matter\n");

    let found = catalog(dir.path(), None);
    assert_eq!(found.len(), 1);
    assert!(!found[0].runnable());
    assert!(found[0]
        .problem
        .as_deref()
        .is_some_and(|p| p.contains("---")));

    let agent = agent_with(&["broken"]);
    assert!(
        granted(&found, &agent).is_empty(),
        "a runbook that cannot start is not offered"
    );
}

#[test]
fn a_workspace_runbook_shadows_the_library_one() {
    let lib = library(&["inbox.triage", "never-send-without-review"]);
    let work = TempDir::new().expect("temp dir");
    write_skill(
        &work.path().join(workspace::CABINET_DIR).join(LIBRARY_DIR),
        "inbox.triage",
        TRIAGE_SEED,
    );

    let found = catalog(lib.path(), Some(work.path()));

    assert_eq!(found.len(), 2, "the hidden one is not listed twice");
    let triage = find(&found, "inbox.triage").expect("found");
    assert_eq!(triage.scope, SkillScope::Workspace);
    assert!(triage.shadows, "the panel can say what is being hidden");
}

/// The per-agent scope. The built-in identity is the assistant from before
/// this phase, so it holds none: a skill is always an explicit grant.
#[test]
fn an_identity_is_offered_only_what_it_was_granted() {
    let dir = library(&["inbox.triage", "never-send-without-review"]);
    let found = catalog(dir.path(), None);

    let triager = agent_with(&["inbox.triage"]);
    let offered: Vec<&str> = granted(&found, &triager)
        .iter()
        .map(|skill| skill.name.as_str())
        .collect();
    assert_eq!(offered, vec!["inbox.triage"]);

    let builtin = Agent::builtin();
    assert_eq!(builtin.id, DEFAULT_AGENT_ID);
    assert!(
        granted(&found, &builtin).is_empty(),
        "the built-in identity holds no skills"
    );
}

/// The catalog carries the line and never the steps. This is the property
/// the phase exists for, asserted on the text that actually gets sent.
#[test]
fn the_catalog_block_carries_no_step_of_any_runbook() {
    let dir = library(&["inbox.triage"]);
    let found = catalog(dir.path(), None);
    let agent = agent_with(&["inbox.triage"]);

    let block = prompt_block(&granted(&found, &agent)).expect("a block");

    assert!(block.contains("inbox.triage"), "{block}");
    assert!(block.contains("v1"), "{block}");
    assert!(block.contains(tool::FS_WRITE), "{block}");
    assert!(
        !block.contains("Rewrite the file whole"),
        "a step reached the system message:\n{block}"
    );
    // Every request carries the catalog, so one line must stay bounded.
    let one = block.len();
    let two = prompt_block(&[&found[0], &found[0]])
        .expect("a block")
        .len();
    let line = two - one;
    assert!(
        line <= doc::SUMMARY_MAX_CHARS + NAME_MAX_CHARS + 64,
        "a second skill costs {line} characters"
    );
}

#[test]
fn an_identity_with_no_skills_gets_no_block_at_all() {
    let dir = library(&["inbox.triage"]);
    let found = catalog(dir.path(), None);

    assert!(prompt_block(&granted(&found, &Agent::builtin())).is_none());
}

/// The body is read when it is invoked, not when the catalog is built.
#[test]
fn the_body_is_read_on_demand_and_reflects_the_file_as_it_is_now() {
    let dir = library(&["inbox.triage"]);
    let found = catalog(dir.path(), None);
    let skill = &found[0];

    assert!(load(skill)
        .expect("loads")
        .body
        .contains("Rewrite the file whole"));

    write_skill(dir.path(), "inbox.triage", REVIEW_SEED);
    assert!(
        load(skill)
            .expect("loads")
            .body
            .contains("Sending is not a step"),
        "an edited runbook is the one that runs"
    );
}

#[test]
fn the_library_is_seeded_once_and_never_argues_about_it() {
    let dir = TempDir::new().expect("temp dir");
    let library = dir.path().join(LIBRARY_DIR);

    seed(&library);
    let seeded = catalog(&library, None);
    let mut names: Vec<&str> = seeded.iter().map(|skill| skill.name.as_str()).collect();
    names.sort_unstable();
    let mut expected: Vec<&str> = SEEDED.iter().map(|(name, _)| *name).collect();
    expected.sort_unstable();
    assert_eq!(names, expected);
    for skill in &seeded {
        assert!(skill.runnable(), "{}: {:?}", skill.name, skill.problem);
    }

    for (name, _) in SEEDED {
        fs::remove_dir_all(library.join(name)).expect("the user deletes it");
    }
    seed(&library);
    assert!(
        catalog(&library, None).is_empty(),
        "a deleted example does not come back"
    );
}

/// A pre-manifest library from the earliest build (no `cos.loop`) still
/// receives [`COS_SKILL`]: the migration checks the disk, not
/// [`SEEDED_BEFORE`].
#[test]
fn a_library_from_before_the_manifest_gains_what_it_was_never_actually_offered() {
    let dir = TempDir::new().expect("temp dir");
    let library = dir.path().join(LIBRARY_DIR);

    // A library exactly as the earliest build left it: one runbook, no
    // manifest, and no `cos.loop` — which that build could not have written.
    write_skill(&library, REVIEW_SKILL, REVIEW_SEED);

    seed(&library);
    let mut names: Vec<String> = catalog(&library, None)
        .into_iter()
        .map(|skill| skill.name)
        .collect();
    names.sort();
    let mut expected: Vec<&str> = SEEDED.iter().map(|(name, _)| *name).collect();
    expected.sort_unstable();
    assert_eq!(names, expected, "including the `cos.loop` it never got");

    // From here the manifest is the record, and a deletion is the user's.
    fs::remove_dir_all(library.join(CHECK_SKILL)).expect("the user deletes it");
    seed(&library);
    assert!(
        !library.join(CHECK_SKILL).exists(),
        "once offered, a runbook is the user's to keep or delete"
    );
}

/// The accepted cost of the disk check: a runbook deleted before the
/// manifest existed comes back once, then a second deletion sticks.
#[test]
fn a_deletion_from_before_the_manifest_comes_back_once_and_then_never_again() {
    let dir = TempDir::new().expect("temp dir");
    let library = dir.path().join(LIBRARY_DIR);

    // Both of the pre-manifest names shipped here, and the owner deleted one.
    write_skill(&library, COS_SKILL, COS_SEED);

    seed(&library);
    assert!(
        library.join(REVIEW_SKILL).is_dir(),
        "it cannot be told apart from one that was never written"
    );

    fs::remove_dir_all(library.join(REVIEW_SKILL)).expect("the user deletes it again");
    seed(&library);
    assert!(
        !library.join(REVIEW_SKILL).exists(),
        "the manifest now records it, so the second deletion is final"
    );
}

#[test]
fn a_run_is_tracked_from_the_envelope_and_closed_by_a_return() {
    let mut active = None;

    let opened = ToolResult::for_test(
        true,
        tool::SKILL_RUN,
        serde_json::json!({ META_SKILL: "inbox.triage" }),
    );
    track(&mut active, tool::SKILL_RUN, &opened);
    assert_eq!(active.as_deref(), Some("inbox.triage"));

    let failed = ToolResult::for_test(false, tool::SKILL_RETURN, serde_json::json!({}));
    track(&mut active, tool::SKILL_RETURN, &failed);
    assert_eq!(
        active.as_deref(),
        Some("inbox.triage"),
        "a refused return leaves the run open"
    );

    let closed = ToolResult::for_test(true, tool::SKILL_RETURN, serde_json::json!({}));
    track(&mut active, tool::SKILL_RETURN, &closed);
    assert_eq!(active, None);
}

#[test]
fn a_declared_tool_the_identity_does_not_hold_is_named() {
    let held = vec![tool::FS_READ.to_owned()];
    let ctx = SkillCtx {
        library: Path::new("."),
        workspace: None,
        tools: &held,
        active: None,
    };

    let declared = vec![tool::FS_READ.to_owned(), tool::FS_WRITE.to_owned()];
    assert_eq!(ctx.missing(&declared), Some(tool::FS_WRITE));
    assert_eq!(ctx.missing(&held), None);
}
