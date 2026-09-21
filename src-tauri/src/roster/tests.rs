use super::*;

use tempfile::TempDir;

use crate::policy::tool;

const CHIEF: &str = "## Chief of Staff\n\n\
    - role: routes work to specialists and keeps the board\n\
    - tools: fs_list, fs_read, fs_write, skill_run, skill_return, handoff_delegate\n\
    - skills: cos.loop\n\
    - runs_per_day: 0\n\n";

const REVIEWER: &str = "## Reviewer\n\n\
    - role: reads what the cabinet produced and says what is wrong with it\n\
    - tools: fs_list, fs_read\n\
    - skills: none\n\
    - runs_per_day: 0\n\n";

fn roster(body: &str) -> String {
    format!("# Roster\n\nRouting for this project.\n\n{body}")
}

struct Fixture {
    _dir: TempDir,
    root: PathBuf,
    data: PathBuf,
    agents: AgentStore,
    audit: AuditLog,
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new().expect("temp dir");
        let data = dir.path().join("data");
        let root = dir.path().join("work");
        fs::create_dir_all(&data).expect("data dir");
        fs::create_dir_all(root.join(ROSTER_DIR)).expect("roster dir");
        let root = dunce::canonicalize(&root).expect("canonical");

        Self {
            agents: AgentStore::load(&data),
            audit: AuditLog::new(&data),
            data,
            root,
            _dir: dir,
        }
    }

    fn propose(&self, text: &str) {
        fs::write(self.root.join(ROSTER_FILE), text).expect("proposal written");
    }

    fn shown(&self, live: &[String]) -> RosterProposal {
        read(&self.root, &self.agents, live, &[]).expect("a proposal")
    }

    fn apply_shown(&self, live: &[String]) -> AppResult<(RosterApplied, Vec<AuditEntry>)> {
        let digest = self.shown(live).digest;
        apply(
            &self.root,
            &self.agents,
            live,
            &[],
            &digest,
            &self.audit,
            "p1",
        )
    }

    fn made(&self, name: &str, tools: &[&str]) -> Agent {
        self.agents
            .create(&AgentDraft {
                name: name.to_owned(),
                role: "made by hand".to_owned(),
                instructions: String::new(),
                provider_id: DEFAULT_PROVIDER_ID.to_owned(),
                model: String::new(),
                tools: tools.iter().map(|tool| (*tool).to_owned()).collect(),
                skills: Vec::new(),
                runs_per_day: 0,
                spend: Default::default(),
            })
            .expect("created")
    }
}

/// The indented roster inside `cabinet.found`'s steps, de-indented.
fn founder_example() -> String {
    skills::FOUND_SEED
        .lines()
        .skip_while(|line| *line != "    # Roster")
        .take_while(|line| line.is_empty() || line.starts_with("    "))
        .map(|line| line.strip_prefix("    ").unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The runbook is the format's documentation. If its example stops
/// parsing, every roster a founder writes from it is refused.
#[test]
fn the_example_in_the_founder_runbook_is_the_default_roster_and_parses() {
    let doc = parse(&founder_example()).expect("the example parses");

    let names: Vec<&str> = doc.identities.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Chief of Staff", "Reviewer"]);

    let chief = &doc.identities[0];
    assert_eq!(chief.runs_per_day, 0, "never on a clock");
    assert!(!chief.tools.contains(&tool::SHELL_EXEC.to_owned()));
    assert!(!chief.tools.contains(&tool::SCREEN_CAPTURE.to_owned()));
    assert!(!chief.tools.contains(&tool::HANDOFF_RETURN.to_owned()));
    assert!(chief.skills.contains(&skills::COS_SKILL.to_owned()));

    let reviewer = &doc.identities[1];
    for refused in [tool::FS_WRITE, tool::SHELL_EXEC, tool::HANDOFF_DELEGATE] {
        assert!(!reviewer.tools.contains(&refused.to_owned()), "{refused}");
    }
    assert!(doc.intended_routines.is_empty(), "`- none` is no routine");
}

/// The default Chief holds every tool its runbooks declare, so nothing it
/// is proposed with fails closed on its first run.
#[test]
fn the_default_roster_holds_every_tool_its_runbooks_call() {
    let dir = TempDir::new().expect("temp dir");
    let library = dir.path().join(skills::LIBRARY_DIR);
    skills::seed(&library);
    let catalog = skills::catalog(&library, None);

    for proposed in parse(&founder_example()).expect("parses").identities {
        assert_eq!(
            notes(&proposed, &catalog),
            Vec::<String>::new(),
            "{}",
            proposed.name
        );
    }
}

#[test]
fn every_identity_names_all_four_fields_and_the_refusal_says_which() {
    for key in KEYS {
        let text = roster(CHIEF).replace(&format!("- {key}:"), "- dropped:");
        let text = text.replace("- dropped:", "Dropped, as prose:");
        let err = parse(&text).expect_err("refused");
        assert!(err.contains(key), "{key}: {err}");
        assert!(err.contains("Chief of Staff"), "{err}");
    }
}

#[test]
fn an_unknown_field_is_refused_with_the_ones_that_work() {
    let err = parse(&roster(&format!("{CHIEF}- instructions: be nice\n"))).expect_err("refused");

    assert!(err.contains("instructions"), "{err}");
    for key in KEYS {
        assert!(err.contains(key), "{err}");
    }
}

#[test]
fn a_field_given_twice_is_refused() {
    let err = parse(&roster(&format!("{CHIEF}- tools: shell_exec\n"))).expect_err("refused");
    assert!(err.contains("twice"), "{err}");
}

#[test]
fn two_identities_with_one_name_are_refused() {
    let err = parse(&roster(&format!(
        "{REVIEWER}{}",
        REVIEWER.replace("Reviewer", "reviewer")
    )))
    .expect_err("refused");
    assert!(err.contains("twice"), "{err}");
}

#[test]
fn a_roster_that_names_nobody_is_refused() {
    let err = parse("# Roster\n\n## Open questions\n\n- who?\n").expect_err("refused");
    assert!(err.contains("no identity"), "{err}");
}

#[test]
fn runs_per_day_is_a_whole_number() {
    let err = parse(&roster(
        &REVIEWER.replace("runs_per_day: 0", "runs_per_day: often"),
    ))
    .expect_err("refused");
    assert!(err.contains("often"), "{err}");
}

#[test]
fn a_dash_line_that_is_not_a_field_is_refused() {
    let err = parse(&roster(&format!("{REVIEWER}- reads diffs carefully\n"))).expect_err("refused");
    assert!(err.contains("not a field"), "{err}");
}

/// Prose is for the reader. A sentence that happens to say "tools:" is not
/// a grant, which is the whole reason fields carry a dash.
#[test]
fn prose_grants_nothing_and_none_is_an_empty_list() {
    let text = roster(&format!(
        "{REVIEWER}It should never get tools: shell_exec, fs_write.\n"
    ));
    let doc = parse(&text).expect("parses");

    assert_eq!(doc.identities[0].tools, [tool::FS_LIST, tool::FS_READ]);
    assert!(doc.identities[0].skills.is_empty());
}

#[test]
fn intended_routines_and_open_questions_are_listed_and_are_not_identities() {
    let text = roster(&format!(
        "{REVIEWER}## Intended routines\n\n- Watch runs watch.digest daily, after one run \
         under watch\n\n## Open questions\n\n- which mailbox?\n- none\n"
    ));
    let doc = parse(&text).expect("parses");

    assert_eq!(doc.identities.len(), 1);
    assert_eq!(doc.intended_routines.len(), 1);
    assert_eq!(doc.open_questions, ["which mailbox?"]);
}

#[test]
fn a_workspace_with_no_roster_has_no_proposal() {
    let f = Fixture::new();
    assert!(read(&f.root, &f.agents, &[], &[]).is_none());

    let err = apply(&f.root, &f.agents, &[], &[], "", &f.audit, "p1").expect_err("refused");
    assert!(err.to_string().contains(ROSTER_FILE), "{err}");
}

/// The exit, at the level of the module: the named identities with the
/// named allow-lists, on the audit log as the operator's act, and nothing
/// else — no routine, no connector, no world.
#[test]
fn apply_creates_the_named_identities_with_the_named_allow_lists_and_nothing_else() {
    let f = Fixture::new();
    f.propose(&roster(&format!(
        "{CHIEF}{REVIEWER}## Intended routines\n\n- Reviewer, weekly\n"
    )));

    let shown = f.shown(&[]);
    assert!(shown.appliable, "{shown:?}");
    assert!(shown
        .entries
        .iter()
        .all(|entry| entry.state == RosterEntryState::New));

    let (applied, lines) = f.apply_shown(&[]).expect("applied");
    assert_eq!(applied.created.len(), 2);
    assert!(applied.skipped.is_empty());

    let chief = f
        .agents
        .list()
        .into_iter()
        .find(|agent| agent.name == "Chief of Staff")
        .expect("created");
    assert_eq!(
        chief.tools,
        [
            tool::FS_LIST,
            tool::FS_READ,
            tool::FS_WRITE,
            tool::SKILL_RUN,
            tool::SKILL_RETURN,
            tool::HANDOFF_DELEGATE,
        ]
    );
    assert_eq!(chief.skills, [skills::COS_SKILL]);
    assert_eq!(chief.runs_per_day, 0);

    assert_eq!(lines.len(), 2);
    for line in &lines {
        assert_eq!(line.tool, AGENT_CREATE);
        assert_eq!(line.decision, AuditDecision::Operator);
        assert!(line.session_id.is_empty(), "no session made it");
        assert!(
            line.args_redacted.contains(ROSTER_FILE),
            "{}",
            line.args_redacted
        );
    }

    assert!(!f.data.join("routines.json").exists(), "no routine");
    assert!(!f.data.join("connectors.json").exists(), "no connector");
    assert!(!f.root.join("world").exists(), "no world");
}

/// A Reviewer somebody made narrow stays narrow, whatever a roster in some
/// other project proposes for the name.
#[test]
fn a_name_that_already_exists_is_skipped_and_never_widened() {
    let f = Fixture::new();
    let narrow = f.made("reviewer", &[tool::FS_READ]);
    f.propose(&roster(&format!(
        "{CHIEF}{}",
        REVIEWER.replace("fs_list, fs_read", "fs_list, fs_read, fs_write, shell_exec")
    )));

    let shown = f.shown(&[]);
    assert_eq!(shown.entries[1].state, RosterEntryState::Present);

    let (applied, _) = f.apply_shown(&[]).expect("applied");
    assert_eq!(applied.skipped, ["Reviewer"]);
    assert_eq!(f.agents.get(&narrow.id).expect("still there"), narrow);
}

#[test]
fn the_builtin_assistant_is_never_a_target() {
    let f = Fixture::new();
    f.propose(&roster(&format!(
        "{CHIEF}{}",
        REVIEWER
            .replace("## Reviewer", "## Assistant")
            .replace("none", "cabinet.found")
    )));

    let shown = f.shown(&[]);
    assert_eq!(shown.entries[1].state, RosterEntryState::Builtin);

    let (applied, _) = f.apply_shown(&[]).expect("applied");
    assert_eq!(applied.skipped, ["Assistant"]);
    assert_eq!(f.agents.list()[0], Agent::builtin());
    assert!(f.agents.list()[0].skills.is_empty());
}

/// Confirming apply is signing what was on screen. A file rewritten after
/// that is not what was signed.
#[test]
fn a_roster_changed_after_it_was_shown_is_not_applied() {
    let f = Fixture::new();
    f.propose(&roster(REVIEWER));
    let shown = f.shown(&[]);

    f.propose(&roster(
        &REVIEWER.replace("fs_list, fs_read", "fs_read, shell_exec"),
    ));
    let err =
        apply(&f.root, &f.agents, &[], &[], &shown.digest, &f.audit, "p1").expect_err("refused");

    assert!(err.to_string().contains("changed"), "{err}");
    assert_eq!(f.agents.list().len(), 1, "nobody was created");
}

#[test]
fn one_identity_that_would_be_refused_means_none_is_created() {
    let f = Fixture::new();
    f.propose(&roster(&format!(
        "{CHIEF}{}",
        REVIEWER.replace("fs_list, fs_read", "fs_read, net_fetch")
    )));

    let shown = f.shown(&[]);
    assert!(!shown.appliable);
    assert!(shown.entries[0].problem.is_none());
    let problem = shown.entries[1].problem.as_deref().unwrap_or_default();
    assert!(problem.contains("net_fetch"), "{problem}");

    let err = f.apply_shown(&[]).expect_err("refused");
    assert!(err.to_string().contains("Nothing was created"), "{err}");
    assert_eq!(f.agents.list().len(), 1);
    assert!(f.audit.tail(10, None).expect("tail").is_empty());
}

#[test]
fn a_connector_tool_nothing_answers_to_is_refused_and_a_live_one_is_granted() {
    let f = Fixture::new();
    f.propose(&roster(
        &REVIEWER.replace("fs_list, fs_read", "fs_read, git__status"),
    ));

    let dead = f.shown(&[]);
    let problem = dead.entries[0].problem.as_deref().unwrap_or_default();
    assert!(problem.contains("git__status"), "{problem}");
    assert!(f.apply_shown(&[]).is_err());

    let live = ["git__status".to_owned()];
    let (applied, _) = f.apply_shown(&live).expect("applied");
    assert_eq!(applied.created[0].tools, [tool::FS_READ, "git__status"]);
}

/// Allowed, the way the form allows it, and said out loud.
#[test]
fn a_runbook_the_identity_could_not_run_is_noted_and_does_not_block() {
    let dir = TempDir::new().expect("temp dir");
    let library = dir.path().join(skills::LIBRARY_DIR);
    skills::seed(&library);
    let catalog = skills::catalog(&library, None);

    let f = Fixture::new();
    f.propose(&roster(&REVIEWER.replace(
        "- tools: fs_list, fs_read\n- skills: none",
        "- tools: fs_list, fs_read, skill_run, skill_return\n- skills: review.diff, not.here",
    )));

    let shown = read(&f.root, &f.agents, &[], &catalog).expect("a proposal");
    assert!(shown.appliable, "{shown:?}");
    let notes = &shown.entries[0].notes;
    assert_eq!(notes.len(), 2, "{notes:?}");
    assert!(notes[0].contains(tool::SHELL_EXEC), "{notes:?}");
    assert!(notes[1].contains("not.here"), "{notes:?}");
}

#[test]
fn a_roster_that_will_not_parse_is_listed_with_the_reason_and_never_applied() {
    let f = Fixture::new();
    f.propose("# Roster\n\nNobody yet.\n");

    let shown = f.shown(&[]);
    assert!(shown.problem.is_some());
    assert!(!shown.appliable);
    assert!(
        !shown.digest.is_empty(),
        "still signed, so a fix is a new digest"
    );

    let err = f.apply_shown(&[]).expect_err("refused");
    assert!(err.to_string().contains("never applied"), "{err}");
}
