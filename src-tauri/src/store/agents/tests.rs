use super::*;

use tempfile::TempDir;

use crate::error::ErrorCode;
use crate::policy::tool;

/// The two tools an identity granted a skill has to hold.
fn skill_tools() -> Vec<String> {
    vec![tool::SKILL_RUN.to_owned(), tool::SKILL_RETURN.to_owned()]
}

/// A draft that passes, so a test can change one field and assert on that
/// field alone.
fn draft(name: &str) -> AgentDraft {
    AgentDraft {
        name: name.to_owned(),
        role: "reviews changes and reports what is risky".to_owned(),
        instructions: "Read before you judge.".to_owned(),
        provider_id: DEFAULT_PROVIDER_ID.to_owned(),
        model: String::new(),
        tools: vec![tool::FS_READ.to_owned(), tool::FS_LIST.to_owned()],
        skills: Vec::new(),
        runs_per_day: AGENT_RUNS_PER_DAY_DEFAULT,
        spend: Default::default(),
    }
}

fn store() -> (TempDir, AgentStore) {
    let dir = TempDir::new().expect("temp dir");
    let store = AgentStore::load(dir.path());
    (dir, store)
}

#[test]
fn payloads_carry_the_documented_field_names() {
    let agent = Agent::builtin();
    let json = serde_json::to_value(&agent).expect("Agent serializes");

    for key in [
        "id",
        "name",
        "role",
        "instructions",
        "provider_id",
        "model",
        "tools",
        "skills",
        "builtin",
    ] {
        assert!(json.get(key).is_some(), "missing `{key}` in {json}");
    }
}

/// The built-in identity has to be exactly the assistant of Phases 5–11,
/// or every session written before this phase changes behaviour.
#[test]
fn the_builtin_identity_holds_every_tool_and_says_nothing_extra() {
    let builtin = Agent::builtin();

    assert_eq!(builtin.id, DEFAULT_AGENT_ID);
    assert!(builtin.builtin);
    assert!(builtin.instructions.is_empty(), "no extra instructions");
    assert!(builtin.role.is_empty());
    assert_eq!(builtin.tools, tools::names());
    for name in tools::names() {
        assert!(builtin.allows(name), "{name} is granted");
    }
}

#[test]
fn a_fresh_install_lists_the_builtin_identity_and_nothing_else() {
    let (_dir, store) = store();

    let listed = store.list();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, DEFAULT_AGENT_ID);
    assert!(!store.path().exists(), "reading writes no document");
}

#[test]
fn an_identity_survives_a_restart() {
    let dir = TempDir::new().expect("temp dir");

    let created = AgentStore::load(dir.path())
        .create(&draft("Reviewer"))
        .expect("created");

    let reopened = AgentStore::load(dir.path());
    let found = reopened.get(&created.id).expect("still there");

    assert_eq!(found, created);
    assert_eq!(reopened.list().len(), 2, "the built-in one and this one");
}

/// The exit condition of the phase, at the level of the store: an identity
/// holds the tools it was granted and no others.
#[test]
fn an_identity_holds_only_the_tools_it_was_granted() {
    let (_dir, store) = store();
    let reviewer = store.create(&draft("Reviewer")).expect("created");

    assert!(reviewer.allows(tool::FS_READ));
    assert!(reviewer.allows(tool::FS_LIST));
    assert!(!reviewer.allows(tool::FS_WRITE));
    assert!(!reviewer.allows(tool::SHELL_EXEC));
    assert!(!reviewer.allows(tool::SCREEN_CAPTURE));
}

/// Registry order, not form order: the schemas the model is shown stay in
/// the order the registry chose.
#[test]
fn granted_tools_are_stored_in_registry_order() {
    let (_dir, store) = store();
    let created = store
        .create(&AgentDraft {
            tools: vec![tool::FS_WRITE.to_owned(), tool::FS_LIST.to_owned()],
            ..draft("Scribe")
        })
        .expect("created");

    assert_eq!(created.tools, vec![tool::FS_LIST, tool::FS_WRITE]);
}

#[test]
fn a_tool_this_build_does_not_have_is_refused_by_name() {
    let (_dir, store) = store();

    let err = store
        .create(&AgentDraft {
            tools: vec!["net_fetch".to_owned()],
            ..draft("Fetcher")
        })
        .expect_err("refused");

    assert_eq!(err.code(), ErrorCode::InvalidSetting);
    assert!(err.to_string().contains("net_fetch"), "{err}");
    // The message names what *is* available, so the fix is on screen.
    assert!(err.to_string().contains(tool::FS_READ), "{err}");
}

#[test]
fn an_identity_with_no_tools_at_all_is_allowed() {
    let (_dir, store) = store();

    let created = store
        .create(&AgentDraft {
            tools: Vec::new(),
            ..draft("Scribe")
        })
        .expect("an identity that only writes prose is a real thing to want");

    assert!(created.tools.is_empty());
    assert!(!created.allows(tool::FS_READ));
}

#[test]
fn names_are_unique_case_insensitively_and_the_builtin_one_counts() {
    let (_dir, store) = store();
    store.create(&draft("Reviewer")).expect("created");

    for taken in ["Reviewer", "reviewer", "Assistant"] {
        let err = store.create(&draft(taken)).expect_err("refused");
        assert_eq!(err.code(), ErrorCode::InvalidSetting);
        assert!(err.to_string().contains(taken), "{err}");
    }
}

#[test]
fn saving_an_identity_under_its_own_name_is_not_a_collision() {
    let (_dir, store) = store();
    let reviewer = store.create(&draft("Reviewer")).expect("created");

    let updated = store
        .update(
            &reviewer.id,
            &AgentDraft {
                role: "reviews changes and files the risks in DECISIONS.md".to_owned(),
                ..draft("Reviewer")
            },
        )
        .expect("its own name is free");

    assert_eq!(
        updated.id, reviewer.id,
        "the id is kept, so sessions stay bound"
    );
    assert!(updated.role.contains("DECISIONS.md"));
}

#[test]
fn an_identity_needs_a_name_and_a_role() {
    let (_dir, store) = store();

    for (field, bad) in [
        (
            "name",
            AgentDraft {
                name: "   ".to_owned(),
                ..draft("x")
            },
        ),
        (
            "role",
            AgentDraft {
                role: String::new(),
                ..draft("Reviewer")
            },
        ),
    ] {
        let err = store.create(&bad).expect_err("refused");
        let json = serde_json::to_value(&err).expect("serializes");
        assert_eq!(json["field"], field, "{json}");
    }
}

/// The prompt stays a policy summary plus what is true now (PLAN 7.1). An
/// identity that has grown a runbook is procedure paid for on every turn;
/// a skill is the same procedure paid for on the turns that use it.
#[test]
fn instructions_are_capped_and_the_message_says_where_a_runbook_goes() {
    let (_dir, store) = store();

    let err = store
        .create(&AgentDraft {
            instructions: "x".repeat(INSTRUCTIONS_MAX_CHARS + 1),
            ..draft("Runbook")
        })
        .expect_err("refused");

    assert!(err.to_string().contains("skill"), "{err}");
}

#[test]
fn a_provider_not_on_file_is_refused() {
    let (_dir, store) = store();

    let err = store
        .create(&AgentDraft {
            provider_id: "anthropic".to_owned(),
            ..draft("Reviewer")
        })
        .expect_err("refused");

    let json = serde_json::to_value(&err).expect("serializes");
    assert_eq!(json["field"], "provider");
    assert!(err.to_string().contains("anthropic"), "{err}");
}

/// PLAN 7.19: any row the roster holds can be bound, with a model of its
/// own or none.
#[test]
fn a_second_row_and_a_model_can_be_bound() {
    let (_dir, store) = store();
    let known = |id: &str| id == DEFAULT_PROVIDER_ID || id == "second";

    let bound = store
        .create_with(
            &AgentDraft {
                provider_id: "second".to_owned(),
                model: "  big-model ".to_owned(),
                ..draft("Reviewer")
            },
            &known,
        )
        .expect("accepted");
    assert_eq!(bound.provider_id, "second");
    assert_eq!(bound.model, "big-model");
    assert_eq!(store.count_for_provider("second"), 1);

    let err = store
        .update_with(
            &bound.id,
            &AgentDraft {
                model: "big model".to_owned(),
                ..draft("Reviewer")
            },
            &known,
        )
        .expect_err("refused");
    assert_eq!(
        serde_json::to_value(&err).expect("serializes")["field"],
        "model"
    );

    let back = store
        .update_with(&bound.id, &draft("Reviewer"), &known)
        .expect("accepted");
    assert_eq!(back.provider_id, DEFAULT_PROVIDER_ID);
    assert!(back.model.is_empty());
    assert_eq!(store.count_for_provider("second"), 0);
}

/// An identity written before PLAN 7.19 reads back with no model: it
/// tracks its row's.
#[test]
fn an_identity_without_a_model_reads_back_empty() {
    let dir = TempDir::new().expect("temp dir");
    fs::write(
        dir.path().join(AGENTS_FILE),
        br#"{"version":1,"agents":[{"id":"a","name":"Old","role":"r","instructions":"","provider_id":"default","tools":[],"created_at":"","updated_at":""}]}"#,
    )
    .expect("write");

    let agent = AgentStore::load(dir.path()).get("a").expect("loaded");
    assert_eq!(agent.model, "");
    assert!(Agent::builtin().model.is_empty());
}

#[test]
fn skill_names_are_the_ones_a_runbook_directory_can_have() {
    let (_dir, store) = store();

    let mut granted = draft("Triager");
    granted.tools.extend(skill_tools());

    let created = store
        .create(&AgentDraft {
            skills: vec![
                "inbox.triage".to_owned(),
                "  ".to_owned(),
                "inbox.triage".to_owned(),
            ],
            ..granted.clone()
        })
        .expect("created");
    assert_eq!(
        created.skills,
        vec!["inbox.triage"],
        "blanks dropped, duplicates collapsed"
    );
    assert!(created.allows_skill("inbox.triage"));
    assert!(!created.allows_skill("deploy.draft"));

    let err = store
        .create(&AgentDraft {
            name: "Other".to_owned(),
            skills: vec!["Inbox Triage".to_owned()],
            ..granted
        })
        .expect_err("refused");
    assert!(err.to_string().contains("inbox.triage"), "{err}");
}

/// A skill an identity cannot load is a grant that does nothing, and the
/// refusal says which tools to tick rather than saving it silently.
#[test]
fn granting_a_skill_without_the_tools_that_run_one_is_refused() {
    let (_dir, store) = store();

    let err = store
        .create(&AgentDraft {
            skills: vec!["inbox.triage".to_owned()],
            ..draft("Triager")
        })
        .expect_err("refused");

    let json = serde_json::to_value(&err).expect("serializes");
    assert_eq!(json["field"], "tools", "the form marks the tick-list");
    assert!(err.to_string().contains(tool::SKILL_RUN), "{err}");
    assert!(err.to_string().contains(tool::SKILL_RETURN), "{err}");
}

/// The built-in identity holds every tool and no skill: it is the
/// assistant from before either allow-list existed, and that assistant had
/// no runbooks.
#[test]
fn the_builtin_identity_runs_no_skill() {
    let builtin = Agent::builtin();

    assert!(builtin.skills.is_empty());
    assert!(!builtin.allows_skill("inbox.triage"));
    assert!(builtin.allows(tool::SKILL_RUN), "it can still load one");
}

#[test]
fn the_builtin_identity_can_be_neither_edited_nor_deleted() {
    let (_dir, store) = store();

    let edited = store
        .update(DEFAULT_AGENT_ID, &draft("Reviewer"))
        .expect_err("refused");
    assert!(edited.to_string().contains("edited"), "{edited}");

    let deleted = store.delete(DEFAULT_AGENT_ID).expect_err("refused");
    assert!(deleted.to_string().contains("deleted"), "{deleted}");

    assert_eq!(store.list().len(), 1, "still there");
}

#[test]
fn deleting_removes_only_the_named_identity() {
    let (_dir, store) = store();
    let reviewer = store.create(&draft("Reviewer")).expect("created");
    let scribe = store
        .create(&AgentDraft { ..draft("Scribe") })
        .expect("created");

    store.delete(&reviewer.id).expect("deleted");

    assert!(store.get(&reviewer.id).is_err());
    assert!(store.get(&scribe.id).is_ok());
    assert!(
        store.delete(&reviewer.id).is_err(),
        "a stale list is reported, not ignored"
    );
}

#[test]
fn a_session_that_named_nothing_resolves_to_the_builtin_identity() {
    let (_dir, store) = store();

    assert_eq!(store.resolve(None), Agent::builtin());
    assert_eq!(store.resolve(Some(DEFAULT_AGENT_ID)), Agent::builtin());
}

/// Only reachable by hand-editing the document. The failure has to be
/// narrow, not wide: a damaged file must never widen what a session can do.
#[test]
fn a_session_naming_an_identity_that_is_gone_can_talk_but_not_act() {
    let (_dir, store) = store();

    let stranded = store.resolve(Some("2f1c-not-on-file"));

    assert_eq!(stranded.id, "2f1c-not-on-file");
    assert!(stranded.tools.is_empty(), "no tools, not every tool");
    for name in tools::names() {
        assert!(!stranded.allows(name), "{name} is not granted");
    }
}

#[test]
fn a_damaged_document_is_moved_aside_instead_of_blocking_startup() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join(AGENTS_FILE);
    fs::write(&path, b"{ this is not json").expect("write");

    let store = AgentStore::load(dir.path());

    assert_eq!(store.list().len(), 1, "the built-in identity still answers");
    assert!(!path.exists(), "the damaged document was moved aside");
    assert!(
        fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().contains("corrupt-")),
        "and kept, rather than deleted"
    );
}

#[test]
fn a_future_schema_version_is_quarantined_rather_than_guessed_at() {
    let dir = TempDir::new().expect("temp dir");
    fs::write(
        dir.path().join(AGENTS_FILE),
        br#"{"version":99,"agents":[]}"#,
    )
    .expect("write");

    let store = AgentStore::load(dir.path());
    assert_eq!(store.list().len(), 1);
    assert!(!dir.path().join(AGENTS_FILE).exists());
}

#[test]
fn a_hand_edited_document_with_a_byte_order_mark_still_loads() {
    let dir = TempDir::new().expect("temp dir");
    let created = AgentStore::load(dir.path())
        .create(&draft("Reviewer"))
        .expect("created");

    let path = dir.path().join(AGENTS_FILE);
    let body = fs::read(&path).expect("read");
    let mut with_bom = vec![0xEF, 0xBB, 0xBF];
    with_bom.extend_from_slice(&body);
    fs::write(&path, with_bom).expect("write");

    assert!(AgentStore::load(dir.path()).get(&created.id).is_ok());
}

/// A roster applied halfway is a cabinet nobody signed (PLAN 7.14).
#[test]
fn creating_a_batch_writes_every_identity_or_none() {
    let (_dir, store) = store();

    let refused = store
        .create_all(&[
            draft("Chief"),
            AgentDraft {
                tools: vec!["net_fetch".to_owned()],
                ..draft("Fetcher")
            },
        ])
        .expect_err("refused");
    assert!(refused.to_string().contains("net_fetch"), "{refused}");
    assert_eq!(store.list().len(), 1, "the Chief was not created either");

    let created = store
        .create_all(&[draft("Chief"), draft("Reviewer")])
        .expect("created");
    assert_eq!(created.len(), 2);
    assert_eq!(
        AgentStore::load(store.path().parent().expect("dir"))
            .list()
            .len(),
        3
    );
}

#[test]
fn a_batch_is_checked_against_itself_as_well_as_the_document() {
    let (_dir, store) = store();
    store.create(&draft("Reviewer")).expect("created");

    let checked = store.check_all(&[draft("Chief"), draft("chief"), draft("REVIEWER")]);

    assert!(checked[0].is_ok());
    assert!(checked[1].is_err(), "the batch's own first Chief counts");
    assert!(checked[2].is_err(), "the document's Reviewer counts");
    assert_eq!(store.list().len(), 2, "checking writes nothing");
}

#[test]
fn a_name_is_taken_case_insensitively_and_the_builtin_one_counts() {
    let (_dir, store) = store();
    store.create(&draft("Reviewer")).expect("created");

    assert!(store.name_taken(" reviewer "));
    assert!(store.name_taken("assistant"));
    assert!(!store.name_taken("Chief of Staff"));
}

#[test]
fn builtin_is_never_written_to_disk() {
    let dir = TempDir::new().expect("temp dir");
    AgentStore::load(dir.path())
        .create(&draft("Reviewer"))
        .expect("created");

    let document = fs::read_to_string(dir.path().join(AGENTS_FILE)).expect("read");
    assert!(!document.contains("builtin"), "{document}");
}
