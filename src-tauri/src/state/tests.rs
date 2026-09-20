use super::*;

use tempfile::TempDir;

/// The library is there from startup with the example runbooks in it, so a
/// fresh install has the format in the place people look for it.
#[test]
fn the_skill_library_is_seeded_on_a_first_run() {
    let dir = TempDir::new().expect("temp dir");
    let state = AppState::new(dir.path());

    assert_eq!(state.skills().parent(), Some(dir.path()));

    let catalog = state.skill_catalog(None);
    let names: Vec<&str> = catalog.iter().map(|skill| skill.name.as_str()).collect();
    assert_eq!(
        names,
        [
            crate::skills::ALERT_SKILL,
            crate::skills::BUDGET_ALERT_SKILL,
            crate::skills::BUDGET_POSITION_SKILL,
            crate::skills::BUDGET_RUNWAY_SKILL,
            crate::skills::FOUND_SKILL,
            crate::skills::COS_SKILL,
            crate::skills::DEPLOY_SKILL,
            crate::skills::MAIL_SKILL,
            crate::skills::REVIEW_SKILL,
            crate::skills::REPLY_SKILL,
            crate::skills::REVENUE_PIPELINE_SKILL,
            crate::skills::REVENUE_THESIS_SKILL,
            crate::skills::REVIEW_DIFF_SKILL,
            crate::skills::SOCIAL_POST_SKILL,
            crate::skills::SOCIAL_REPLY_SKILL,
            crate::skills::SOCIAL_SCAN_SKILL,
            crate::skills::THREAD_SKILL,
            crate::skills::WATCH_DIGEST_SKILL,
            crate::skills::WATCH_IMPACT_SKILL,
            crate::skills::WATCH_SWEEP_SKILL,
            crate::skills::WISH_LIST_SKILL,
            crate::skills::CHECK_SKILL,
            crate::skills::DRAFT_SKILL,
            crate::skills::PERCEIVE_SKILL,
            crate::skills::VERIFY_SKILL,
        ]
    );
    for skill in &catalog {
        assert!(skill.runnable(), "{}: {:?}", skill.name, skill.problem);
    }

    // And no identity is offered it until someone grants it: the built-in
    // one is the assistant from before skills existed.
    assert!(crate::skills::granted(&catalog, &Agent::builtin()).is_empty());
}

/// Captures go beside the stores and never into a workspace (PLAN 5.4).
/// The directory exists from startup, because the asset-protocol scope in
/// `lib.rs` is registered against it before anything has been captured.
#[test]
fn the_capture_directory_is_ready_before_anything_is_captured() {
    let dir = TempDir::new().expect("temp dir");
    let state = AppState::new(dir.path());

    assert!(state.captures().is_dir(), "created at startup");
    assert!(
        state
            .captures()
            .starts_with(dunce::canonicalize(dir.path()).expect("canonical data directory")),
        "under the application-data directory"
    );
    assert_eq!(
        state.captures().file_name().and_then(|n| n.to_str()),
        Some("captures")
    );
}

#[test]
fn the_tray_is_absent_until_setup_marks_it() {
    let dir = TempDir::new().expect("temp dir");
    let state = AppState::new(dir.path());
    assert!(
        !state.has_tray(),
        "a missing tray must not trap close-to-hide"
    );
    state.mark_tray();
    assert!(state.has_tray());
}

#[test]
fn quitting_latches_once() {
    let dir = TempDir::new().expect("temp dir");
    let state = AppState::new(dir.path());
    assert!(!state.is_quitting());
    assert!(state.begin_quit(), "first call wins");
    assert!(state.is_quitting());
    assert!(!state.begin_quit(), "second call is a no-op");
    assert!(state.is_quitting());
}

/// The audit log the runtime hands out writes into the data directory.
#[test]
fn the_audit_log_writes_into_the_data_directory() {
    let dir = TempDir::new().expect("temp dir");
    let state = AppState::new(dir.path());

    let log = state.audit();
    assert_eq!(log.path().parent(), Some(dir.path()));

    log.append(&crate::audit::AuditRecord {
        session_id: "s1",
        agent_id: DEFAULT_AGENT_ID,
        turn_id: "t1",
        call_id: "c1",
        tool: "fs_list",
        skill: "",
        handoff: "",
        routine: "",
        decision: crate::audit::AuditDecision::Auto,
        policy_reason: "a read-only listing inside the workspace",
        args: &serde_json::json!({ "path": "." }),
        outcome: crate::audit::Outcome::Ok,
        duration_ms: 1,
        bytes_in: 0,
        bytes_out: 4,
        error_code: None,
        artifact: None,
    });

    assert_eq!(log.tail(10, None).expect("tail").len(), 1);
    assert!(log.path().is_file(), "the line reached the data directory");
}

/// The provider follows the settings: scripted when unconfigured, the real
/// one once a URL and model are set. Nothing here writes a key.
///
/// Only rows whose kind is a CLI login are added below, so no test reads a
/// non-default keyring account.
#[test]
fn the_provider_follows_the_settings() {
    let dir = TempDir::new().expect("temp dir");
    let state = AppState::new(dir.path());

    assert_eq!(
        state.provider().model(),
        crate::agent::provider::fake::FAKE_MODEL,
        "an unconfigured install still answers"
    );

    state
        .settings()
        .set(
            "https://api.example.test/v1",
            "some-model",
            crate::store::AuthKind::ApiKey,
            None,
        )
        .expect("accepted");

    assert_eq!(
        state.provider().model(),
        "some-model",
        "a configured provider answers as itself, with or without a key"
    );
}

/// The panel is shown a source and a hint, never a key — and an
/// unconfigured install has nothing to hint at.
#[test]
fn masked_settings_carry_no_key() {
    let dir = TempDir::new().expect("temp dir");
    let state = AppState::new(dir.path());

    let masked = state.masked_settings();
    assert_eq!(masked.providers.len(), 1);
    assert_eq!(masked.providers[0].id, DEFAULT_PROVIDER_ID);
    assert_eq!(masked.providers[0].base_url, "");
    assert_eq!(masked.providers[0].model, "");
    assert_eq!(
        masked.providers[0].auth_kind,
        crate::store::AuthKind::ApiKey
    );

    let rendered = serde_json::to_string(&masked).expect("serializes");
    assert!(
        !rendered.contains("\"api_key\":"),
        "the payload must not have a key field: {rendered}"
    );

    // PLAN 7.18: the decision half is masked the same way.
    assert!(masked.decision.annotate_approvals);
    assert_eq!(masked.decision.default_model, "jev-latest");
    assert!(!rendered.contains("typesafe-api-key"), "{rendered}");
}

/// Grants and the audit log are per-process, not per-store: a second
/// `AppState` over the same directory must find the lines the first one
/// wrote, and none of its grants.
#[test]
fn a_restart_keeps_the_log_and_drops_the_grants() {
    let dir = TempDir::new().expect("temp dir");

    let first = AppState::new(dir.path());
    first.grants().insert("s1", crate::policy::Grant::FsWrite);
    first.audit().append(&crate::audit::AuditRecord {
        session_id: "s1",
        agent_id: DEFAULT_AGENT_ID,
        turn_id: "t1",
        call_id: "c1",
        tool: "fs_write",
        skill: "",
        handoff: "",
        routine: "",
        decision: crate::audit::AuditDecision::AllowOnce,
        policy_reason: "this creates a file in the workspace",
        args: &serde_json::json!({ "path": "a.txt", "content": "x" }),
        outcome: crate::audit::Outcome::Ok,
        duration_ms: 1,
        bytes_in: 1,
        bytes_out: 0,
        error_code: None,
        artifact: None,
    });

    let second = AppState::new(dir.path());

    assert_eq!(
        second.audit().tail(10, None).expect("tail").len(),
        1,
        "the record of what was done outlives the process"
    );
    assert!(
        !second.grants().holds("s1", &crate::policy::Grant::FsWrite),
        "an allow-session grant must never survive a restart"
    );
}

/// A second row as a CLI login: keyless, so no test reaches the
/// credential store for it.
fn cli_row(state: &AppState, model: &str) -> String {
    state
        .settings()
        .add(&RowDraft {
            label: Some("Claude"),
            base_url: "",
            model,
            auth_kind: AuthKind::ClaudeCli,
            max_output_tokens: None,
        })
        .expect("added")
        .id
}

fn reviewer_on(state: &AppState, provider_id: &str, model: &str) -> Agent {
    state
        .create_agent(&AgentDraft {
            name: "Reviewer".to_owned(),
            role: "reads and reports".to_owned(),
            instructions: String::new(),
            provider_id: provider_id.to_owned(),
            model: model.to_owned(),
            tools: Vec::new(),
            skills: Vec::new(),
            runs_per_day: 0,
        })
        .expect("created")
}

/// PLAN 7.19's exit, end to end below the commands: an identity bound to a
/// second row answers from it, a session overrides that without touching
/// the identity, and clearing the override goes back.
#[test]
fn a_turn_answers_from_the_resolved_binding() {
    let dir = TempDir::new().expect("temp dir");
    let state = AppState::new(dir.path());
    let second = cli_row(&state, "claude-sonnet-4-6");
    let reviewer = reviewer_on(&state, &second, "claude-opus-4-1");
    let session = state
        .create_session("p1", None, Some(&reviewer.id), None, None)
        .expect("session");

    assert_eq!(
        state.provider_for(&reviewer, &session.id).model(),
        "claude-opus-4-1",
        "the identity's model"
    );

    let bound = state
        .set_session_binding(&session.id, None, Some("claude-haiku-4-5"))
        .expect("bound");
    assert_eq!(bound.model.as_deref(), Some("claude-haiku-4-5"));
    assert_eq!(bound.agent_id, reviewer.id, "the identity stays");
    assert_eq!(
        state.provider_for(&reviewer, &session.id).model(),
        "claude-haiku-4-5"
    );

    state
        .set_session_binding(&session.id, Some(DEFAULT_PROVIDER_ID), None)
        .expect("bound");
    assert_eq!(
        state.provider_for(&reviewer, &session.id).model(),
        crate::agent::provider::fake::FAKE_MODEL,
        "the unconfigured default row is the scripted provider"
    );

    let cleared = state
        .set_session_binding(&session.id, Some("  "), Some(""))
        .expect("cleared");
    assert_eq!((cleared.provider_id, cleared.model), (None, None));
    assert_eq!(
        state.provider_for(&reviewer, &session.id).model(),
        "claude-opus-4-1"
    );
    assert_eq!(
        state.agents().get(&reviewer.id).expect("kept"),
        reviewer,
        "no override rebinds the identity"
    );
}

#[test]
fn a_session_can_be_created_with_an_override() {
    let dir = TempDir::new().expect("temp dir");
    let state = AppState::new(dir.path());
    let second = cli_row(&state, "claude-sonnet-4-6");

    let session = state
        .create_session("p1", None, None, Some(&second), None)
        .expect("session");
    assert_eq!(session.provider_id.as_deref(), Some(second.as_str()));
    assert_eq!(session.model, None);
    assert_eq!(
        state.provider_for(&Agent::builtin(), &session.id).model(),
        "claude-sonnet-4-6",
        "a row with no model override sends the row's"
    );

    let err = state
        .create_session("p1", None, None, Some("gone"), None)
        .expect_err("refused");
    assert_eq!(
        serde_json::to_value(&err).expect("serializes")["field"],
        "provider"
    );
}

#[test]
fn a_binding_cannot_change_while_a_turn_runs() {
    let dir = TempDir::new().expect("temp dir");
    let state = AppState::new(dir.path());
    let session = state
        .create_session("p1", None, None, None, None)
        .expect("session");
    let _cancel = state.turns().begin(&session.id, "t1").expect("begun");

    let err = state
        .set_session_binding(&session.id, None, Some("m"))
        .expect_err("refused");
    assert_eq!(
        serde_json::to_value(&err).expect("serializes")["code"],
        "E_TURN_BUSY"
    );
    assert_eq!(
        state.sessions().binding_of(&session.id).expect("read"),
        (None, None)
    );
}

#[test]
fn an_identity_cannot_name_a_row_that_is_not_on_file() {
    let dir = TempDir::new().expect("temp dir");
    let state = AppState::new(dir.path());
    let second = cli_row(&state, "m");
    let reviewer = reviewer_on(&state, &second, "");

    let err = state
        .update_agent(
            &reviewer.id,
            &AgentDraft {
                name: reviewer.name.clone(),
                role: reviewer.role.clone(),
                instructions: String::new(),
                provider_id: "gone".to_owned(),
                model: String::new(),
                tools: Vec::new(),
                skills: Vec::new(),
                runs_per_day: 0,
            },
        )
        .expect_err("refused");
    assert_eq!(
        serde_json::to_value(&err).expect("serializes")["field"],
        "provider"
    );
}

/// A row something answers from is not deleted, and nothing is moved to
/// another row to make room.
#[test]
fn a_row_in_use_is_not_deleted() {
    let dir = TempDir::new().expect("temp dir");
    let state = AppState::new(dir.path());

    assert!(state.delete_provider(DEFAULT_PROVIDER_ID).is_err());

    let bound = cli_row(&state, "m");
    let reviewer = reviewer_on(&state, &bound, "");
    let overridden = cli_row(&state, "m");
    let session = state
        .create_session("p1", None, None, Some(&overridden), None)
        .expect("session");

    let err = state.delete_provider(&bound).expect_err("refused");
    assert!(
        matches!(
            err,
            AppError::ProviderInUse {
                identities: 1,
                sessions: 0
            }
        ),
        "{err}"
    );
    let err = state.delete_provider(&overridden).expect_err("refused");
    assert!(
        matches!(
            err,
            AppError::ProviderInUse {
                identities: 0,
                sessions: 1
            }
        ),
        "{err}"
    );
    assert_eq!(
        state.agents().get(&reviewer.id).expect("kept").provider_id,
        bound
    );

    state
        .set_session_binding(&session.id, None, None)
        .expect("cleared");
    state.delete_provider(&overridden).expect("deleted");
    assert!(!state.settings().contains(&overridden));
}

/// Clearing a key a CLI owns is refused: Aegis never stored it.
#[test]
fn a_cli_row_has_no_key_to_clear() {
    let dir = TempDir::new().expect("temp dir");
    let state = AppState::new(dir.path());
    let second = cli_row(&state, "m");

    let err = state.clear_provider_key(&second).expect_err("refused");
    assert_eq!(
        serde_json::to_value(&err).expect("serializes")["code"],
        "E_INVALID_SETTING"
    );
    assert!(matches!(
        state.clear_provider_key("gone"),
        Err(AppError::ProviderNotFound { .. })
    ));
}
