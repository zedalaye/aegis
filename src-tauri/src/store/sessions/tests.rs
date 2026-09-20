use super::*;

use tempfile::TempDir;

/// Every session idle — the state a store built in a test is in.
fn idle(_id: &str) -> SessionState {
    SessionState::Idle
}

/// A store plus the directory it lives in.
struct Fixture {
    _dir: TempDir,
    data: PathBuf,
    store: SessionStore,
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new().expect("temp dir");
        let data = dir.path().join("data");
        fs::create_dir_all(&data).expect("data dir");
        let store = SessionStore::load(&data);
        Self {
            _dir: dir,
            data,
            store,
        }
    }

    fn document(&self) -> PathBuf {
        self.data.join(SESSIONS_FILE)
    }

    /// Reloads from disk, as a restart would.
    fn reopen(&self) -> SessionStore {
        SessionStore::load(&self.data)
    }
}

/// A session override survives a restart, is absent from the document
/// while it inherits, and clearing it returns to inheriting (PLAN 7.19).
#[test]
fn a_binding_override_survives_a_restart_and_clears() {
    let fx = Fixture::new();
    let plain = fx
        .store
        .create("p1", None, DEFAULT_AGENT_ID)
        .expect("create");
    let bound = fx
        .store
        .create_bound("p1", None, DEFAULT_AGENT_ID, Some("second"), None)
        .expect("create");

    let written = fs::read_to_string(fx.document()).expect("the document");
    assert_eq!(
        written.matches("\"provider_id\"").count(),
        1,
        "only the overriding session writes the key: {written}"
    );

    fx.store
        .set_binding(&plain.id, None, Some("small"), SessionState::Idle)
        .expect("bound");
    assert_eq!(fx.store.count_for_provider("second"), 1);

    let reopened = fx.reopen();
    assert_eq!(
        reopened.binding_of(&plain.id).expect("read"),
        (None, Some("small".to_owned()))
    );
    assert_eq!(
        reopened.binding_of(&bound.id).expect("read"),
        (Some("second".to_owned()), None)
    );

    let cleared = reopened
        .set_binding(&bound.id, None, None, SessionState::Idle)
        .expect("cleared");
    assert_eq!((cleared.provider_id, cleared.model), (None, None));
    assert_eq!(reopened.count_for_provider("second"), 0);
}

#[test]
fn what_a_turn_spent_survives_a_restart_and_sums_on_the_row() {
    let fx = Fixture::new();
    let created = fx
        .store
        .create("p1", Some("Refactor"), DEFAULT_AGENT_ID)
        .expect("create");

    fx.store
        .charge(&created.id, TurnCost::reported("t1", 900, 120))
        .expect("charge");
    fx.store
        .charge(&created.id, TurnCost::reported("t2", 1_100, 80))
        .expect("charge");

    let reopened = fx.reopen();
    let row = reopened
        .summary(&created.id, SessionState::Idle)
        .expect("row");
    assert_eq!(row.cost.turns, 2);
    assert_eq!(row.cost.prompt_tokens, 2_000);
    assert_eq!(row.cost.completion_tokens, 200);
    assert_eq!(row.cost.total(), 2_200);
    assert_eq!(row.cost.unreported, 0);

    let turns = reopened.costs(&created.id).expect("the per-turn rows");
    assert_eq!(turns.len(), 2, "the join a trace makes is on the turn");
    assert_eq!(turns[0].turn_id, "t1", "oldest first");
}

#[test]
fn the_cached_share_survives_a_restart_and_sums_with_the_rest() {
    let fx = Fixture::new();
    let created = fx
        .store
        .create("p1", Some("Read the PDFs"), DEFAULT_AGENT_ID)
        .expect("create");

    fx.store
        .charge(
            &created.id,
            TurnCost::reported("t1", 20_000, 400).with_cache(0, 19_000),
        )
        .expect("charge");
    // The second turn is what caching is for: a bigger prompt that cost
    // less, because almost all of it was the first turn's cache entry.
    fx.store
        .charge(
            &created.id,
            TurnCost::reported("t2", 24_000, 300).with_cache(19_000, 4_500),
        )
        .expect("charge");

    let reopened = fx.reopen();
    let row = reopened
        .summary(&created.id, SessionState::Idle)
        .expect("row");
    assert_eq!(row.cost.prompt_tokens, 44_000);
    assert_eq!(row.cost.cache_read_tokens, 19_000);
    assert_eq!(row.cost.cache_creation_tokens, 23_500);
}

#[test]
fn a_turn_charged_before_the_cache_was_counted_still_reads_back() {
    // The two fields are `serde(default)`, so a session file written by an
    // earlier build is a session whose turns cached nothing — not a store
    // that refuses to load.
    let earlier = serde_json::json!({
        "turn_id": "t1",
        "prompt_tokens": 900,
        "completion_tokens": 120,
        "reported": true,
        "at": "2026-09-02T10:00:00Z",
    });

    let cost: TurnCost = serde_json::from_value(earlier).expect("an earlier turn still loads");
    assert_eq!(cost.prompt_tokens, 900);
    assert_eq!(cost.cache_read_tokens, 0);
    assert_eq!(cost.cache_creation_tokens, 0);
    assert!(cost.reported);
}

#[test]
fn a_turn_whose_provider_said_nothing_is_unknown_rather_than_free() {
    let fx = Fixture::new();
    let created = fx
        .store
        .create("p1", None, DEFAULT_AGENT_ID)
        .expect("create");

    fx.store
        .charge(&created.id, TurnCost::reported("t1", 10, 5))
        .expect("charge");
    let total = fx
        .store
        .charge(&created.id, TurnCost::unreported("t2"))
        .expect("charge");

    assert_eq!(total.turns, 2);
    assert_eq!(total.unreported, 1);
    assert_eq!(
        total.total(),
        15,
        "at least this many tokens were spent, and the row says how much of \
         itself is missing"
    );
}

#[test]
fn charging_a_turn_twice_corrects_it_rather_than_doubling_it() {
    let fx = Fixture::new();
    let created = fx
        .store
        .create("p1", None, DEFAULT_AGENT_ID)
        .expect("create");

    fx.store
        .charge(&created.id, TurnCost::reported("t1", 10, 5))
        .expect("charge");
    let total = fx
        .store
        .charge(&created.id, TurnCost::reported("t1", 20, 5))
        .expect("charge again");

    assert_eq!(total.turns, 1);
    assert_eq!(total.total(), 25);
}

#[test]
fn a_cost_does_not_reorder_the_sidebar() {
    let fx = Fixture::new();
    let created = fx
        .store
        .create("p1", Some("Refactor"), DEFAULT_AGENT_ID)
        .expect("create");
    let before = created.updated_at.clone();

    fx.store
        .charge(&created.id, TurnCost::reported("t1", 10, 5))
        .expect("charge");

    let row = fx
        .store
        .summary(&created.id, SessionState::Idle)
        .expect("row");
    assert_eq!(
        row.updated_at, before,
        "bookkeeping is not activity; the turn already bumped this"
    );
}

#[test]
fn a_session_written_before_costs_existed_reads_back_with_none() {
    let fx = Fixture::new();
    let created = fx
        .store
        .create("p1", None, DEFAULT_AGENT_ID)
        .expect("create");

    // The document as an earlier build wrote it: no `costs` key at all.
    let mut document: serde_json::Value =
        serde_json::from_slice(&fs::read(fx.document()).expect("read")).expect("parse");
    document["sessions"][0]
        .as_object_mut()
        .expect("a session object")
        .remove("costs");
    fs::write(
        fx.document(),
        serde_json::to_vec_pretty(&document).expect("serialize"),
    )
    .expect("write");

    let row = fx
        .reopen()
        .summary(&created.id, SessionState::Idle)
        .expect("the older document still loads");
    assert!(
        row.cost.is_empty(),
        "nothing was recorded, so nothing is claimed"
    );
}

#[test]
fn a_session_survives_a_restart() {
    let fx = Fixture::new();
    let created = fx
        .store
        .create("p1", Some("Refactor"), DEFAULT_AGENT_ID)
        .expect("create");
    fx.store
        .append(&created.id, Message::user("hello"), SessionState::Idle)
        .expect("append");

    let reopened = fx.reopen();
    let detail = reopened
        .open(&created.id, SessionState::Idle)
        .expect("open");

    assert_eq!(detail.session.id, created.id);
    assert_eq!(detail.session.title, "Refactor");
    assert_eq!(detail.messages.len(), 1);
    assert_eq!(detail.messages[0].text, "hello");
}

/// The one invariant the module exists to protect: a process killed
/// mid-turn must not come back claiming the turn is still running.
#[test]
fn a_restart_never_reports_a_session_as_running() {
    let fx = Fixture::new();
    let created = fx
        .store
        .create("p1", None, DEFAULT_AGENT_ID)
        .expect("create");
    fx.store
        .append(&created.id, Message::user("go"), SessionState::Running)
        .expect("append");

    // Whatever state was stamped on the returned summary, nothing about it
    // reached the document.
    let raw = fs::read_to_string(fx.document()).expect("document");
    assert!(
        !raw.contains("running"),
        "session state must never be persisted: {raw}"
    );

    let listed = fx.reopen().list("p1", &idle);
    assert_eq!(listed[0].state, SessionState::Idle);
}

#[test]
fn the_first_user_message_names_an_unnamed_session() {
    let fx = Fixture::new();
    let created = fx
        .store
        .create("p1", None, DEFAULT_AGENT_ID)
        .expect("create");
    assert_eq!(created.title, DEFAULT_TITLE);

    let updated = fx
        .store
        .append(
            &created.id,
            Message::user("  Explain the   policy matrix\nplease  "),
            SessionState::Idle,
        )
        .expect("append");

    assert_eq!(updated.title, "Explain the policy matrix please");
}

#[test]
fn a_session_the_user_named_keeps_its_name() {
    let fx = Fixture::new();
    let created = fx
        .store
        .create("p1", Some("Mine"), DEFAULT_AGENT_ID)
        .expect("create");

    let updated = fx
        .store
        .append(&created.id, Message::user("anything"), SessionState::Idle)
        .expect("append");

    assert_eq!(updated.title, "Mine");
}

#[test]
fn a_long_first_message_is_cut_on_a_word_boundary() {
    let title = title_from(
        "Please read every file under src and tell me which of them still \
         mention the old name",
    );

    assert!(title.ends_with('…'), "{title}");
    assert!(title.chars().count() <= TITLE_MAX_CHARS + 1, "{title}");
    assert!(
        !title.trim_end_matches('…').ends_with(' '),
        "the ellipsis follows a word, not a space: {title}"
    );
    assert!(
        title.starts_with("Please read every file under src"),
        "{title}"
    );
}

/// A word longer than the budget has no boundary to fall back on; cutting
/// it mid-word is the only option, and must not panic on a multi-byte
/// character.
#[test]
fn a_title_never_splits_a_character() {
    let title = title_from(&"é".repeat(200));
    assert_eq!(title.chars().count(), TITLE_MAX_CHARS + 1);
}

#[test]
fn sessions_list_most_recently_active_first() {
    let fx = Fixture::new();
    let first = fx
        .store
        .create("p1", Some("First"), DEFAULT_AGENT_ID)
        .expect("create");
    let second = fx
        .store
        .create("p1", Some("Second"), DEFAULT_AGENT_ID)
        .expect("create");
    let other = fx
        .store
        .create("p2", Some("Elsewhere"), DEFAULT_AGENT_ID)
        .expect("create");

    // Touching the older session moves it to the top.
    fx.store
        .append(&first.id, Message::user("later"), SessionState::Idle)
        .expect("append");

    let listed = fx.store.list("p1", &idle);
    let titles: Vec<&str> = listed.iter().map(|s| s.title.as_str()).collect();

    assert_eq!(titles, vec!["First", "Second"]);
    assert!(
        !listed.iter().any(|s| s.id == other.id),
        "another project's sessions are not this project's"
    );
    assert_eq!(listed[0].message_count, 1);
    assert_eq!(listed[1].id, second.id);
}

#[test]
fn the_live_state_comes_from_the_caller() {
    let fx = Fixture::new();
    let created = fx
        .store
        .create("p1", None, DEFAULT_AGENT_ID)
        .expect("create");
    let id = created.id.clone();

    let running = |candidate: &str| {
        if candidate == id {
            SessionState::Running
        } else {
            SessionState::Idle
        }
    };

    assert_eq!(
        fx.store.list("p1", &running)[0].state,
        SessionState::Running
    );
}

#[test]
fn renaming_refuses_an_empty_title() {
    let fx = Fixture::new();
    let created = fx
        .store
        .create("p1", Some("Keep"), DEFAULT_AGENT_ID)
        .expect("create");

    let err = fx
        .store
        .rename(&created.id, "   ", SessionState::Idle)
        .expect_err("an empty title is refused");
    assert!(matches!(err, AppError::SessionTitle), "{err:?}");

    let detail = fx
        .store
        .open(&created.id, SessionState::Idle)
        .expect("open");
    assert_eq!(detail.session.title, "Keep");
}

#[test]
fn a_tool_call_status_can_be_advanced_without_losing_its_summary() {
    let fx = Fixture::new();
    let created = fx
        .store
        .create("p1", None, DEFAULT_AGENT_ID)
        .expect("create");

    fx.store
        .append(
            &created.id,
            Message::assistant(
                "",
                vec![ToolCallRecord {
                    call_id: "call_1".to_owned(),
                    tool: "fs_read".to_owned(),
                    args_json: r#"{"path":"a.txt"}"#.to_owned(),
                    status: ToolCallStatus::Pending,
                    summary: None,
                    image_path: None,
                    thought_signature: None,
                }],
            ),
            SessionState::Running,
        )
        .expect("append");

    assert!(fx
        .store
        .set_tool_call_status(
            &created.id,
            "call_1",
            ToolCallStatus::Ok,
            Some("read a.txt (12 B)".to_owned()),
            None,
        )
        .expect("update"));

    // A later status change that carries no summary keeps the one on file.
    assert!(fx
        .store
        .set_tool_call_status(&created.id, "call_1", ToolCallStatus::Cancelled, None, None)
        .expect("update"));

    let detail = fx
        .store
        .open(&created.id, SessionState::Idle)
        .expect("open");
    let call = &detail.messages[0].tool_calls[0];
    assert_eq!(call.status, ToolCallStatus::Cancelled);
    assert_eq!(call.summary.as_deref(), Some("read a.txt (12 B)"));
}

#[test]
fn updating_an_unknown_call_reports_it_rather_than_failing() {
    let fx = Fixture::new();
    let created = fx
        .store
        .create("p1", None, DEFAULT_AGENT_ID)
        .expect("create");

    let found = fx
        .store
        .set_tool_call_status(&created.id, "nope", ToolCallStatus::Ok, None, None)
        .expect("no error");
    assert!(!found);
}

#[test]
fn deleting_a_project_takes_its_sessions_with_it() {
    let fx = Fixture::new();
    fx.store
        .create("p1", None, DEFAULT_AGENT_ID)
        .expect("create");
    fx.store
        .create("p1", None, DEFAULT_AGENT_ID)
        .expect("create");
    let kept = fx
        .store
        .create("p2", None, DEFAULT_AGENT_ID)
        .expect("create");

    assert_eq!(fx.store.delete_for_project("p1").expect("delete"), 2);
    assert!(fx.store.list("p1", &idle).is_empty());
    assert_eq!(fx.store.list("p2", &idle).len(), 1);
    assert!(fx.store.open(&kept.id, SessionState::Idle).is_ok());
}

#[test]
fn an_unknown_session_is_a_stale_list_not_a_crash() {
    let fx = Fixture::new();

    let err = fx
        .store
        .open("missing", SessionState::Idle)
        .expect_err("no such session");
    assert!(matches!(err, AppError::SessionNotFound { .. }), "{err:?}");

    assert!(fx.store.delete("missing").is_err());
    assert!(fx.store.messages("missing").is_err());
    assert!(fx.store.project_of("missing").is_err());
}

#[test]
fn a_damaged_document_is_moved_aside_rather_than_blocking_the_app() {
    let fx = Fixture::new();
    fx.store
        .create("p1", Some("Gone"), DEFAULT_AGENT_ID)
        .expect("create");

    fs::write(fx.document(), b"{ not json").expect("damage the document");
    let reopened = fx.reopen();

    assert!(reopened.list("p1", &idle).is_empty());
    let quarantined: Vec<_> = fs::read_dir(&fx.data)
        .expect("read data dir")
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains("corrupt-"))
        .collect();
    assert_eq!(quarantined.len(), 1, "the original is kept, not deleted");
}

#[test]
fn a_future_schema_version_is_quarantined_rather_than_guessed_at() {
    let fx = Fixture::new();
    fs::write(
        fx.document(),
        br#"{"version":99,"sessions":[{"id":"s","project_id":"p","title":"t","created_at":"","updated_at":"","messages":[]}]}"#,
    )
    .expect("write a future document");

    assert!(fx.reopen().list("p", &idle).is_empty());
}

/// Generated bindings follow a rename silently; this pins the wire names
/// to PLAN 2.1.
#[test]
fn payloads_carry_the_documented_field_names() {
    let detail = SessionDetail {
        session: SessionSummary {
            id: "s".to_owned(),
            project_id: "p".to_owned(),
            agent_id: DEFAULT_AGENT_ID.to_owned(),
            provider_id: None,
            model: None,
            title: "First".to_owned(),
            created_at: "2026-08-28T09:41:07.412Z".to_owned(),
            updated_at: "2026-08-28T09:41:07.412Z".to_owned(),
            message_count: 3,
            state: SessionState::AwaitingApproval,
            delegated: None,
            scheduled: None,
            cost: Cost::default(),
        },
        messages: vec![Message::assistant(
            "done",
            vec![ToolCallRecord {
                call_id: "call_1".to_owned(),
                tool: "fs_read".to_owned(),
                args_json: "{}".to_owned(),
                status: ToolCallStatus::Ok,
                summary: None,
                image_path: None,
                thought_signature: None,
            }],
        )],
        compaction: Some(Compaction {
            through_message_id: "m1".to_owned(),
            folded: 6,
            state: "Goal: ship it".to_owned(),
            at: "2026-08-28T09:41:07.412Z".to_owned(),
        }),
        pending_approvals: Vec::new(),
    };

    let json = serde_json::to_value(&detail).expect("SessionDetail serializes");

    let fields = |value: &serde_json::Value| -> Vec<String> {
        let mut keys: Vec<String> = value
            .as_object()
            .expect("a JSON object")
            .keys()
            .cloned()
            .collect();
        keys.sort();
        keys
    };
    let sorted = |names: &[&str]| -> Vec<String> {
        let mut out: Vec<String> = names.iter().map(|s| (*s).to_owned()).collect();
        out.sort();
        out
    };

    assert_eq!(
        fields(&json),
        sorted(&["session", "messages", "compaction", "pending_approvals"])
    );
    assert_eq!(
        fields(&json["compaction"]),
        sorted(&["through_message_id", "folded", "state", "at"])
    );
    assert_eq!(
        fields(&json["session"]),
        sorted(&[
            "id",
            "project_id",
            "agent_id",
            "provider_id",
            "model",
            "title",
            "created_at",
            "updated_at",
            "message_count",
            "state",
            "delegated",
            "scheduled",
            "cost",
        ])
    );
    assert_eq!(
        fields(&json["messages"][0]),
        sorted(&[
            "id",
            "role",
            "text",
            "tool_calls",
            "tool_call_id",
            "created_at"
        ])
    );
    assert_eq!(
        fields(&json["messages"][0]["tool_calls"][0]),
        sorted(&[
            "call_id",
            "tool",
            "args_json",
            "status",
            "summary",
            "image_path"
        ])
    );

    // The enums cross the wire as the snake_case strings the UI branches on.
    assert_eq!(json["session"]["state"], "awaiting_approval");
    assert_eq!(json["messages"][0]["role"], "assistant");
    assert_eq!(json["messages"][0]["tool_calls"][0]["status"], "ok");

    // `None` must reach TypeScript as `null`, not as a missing key.
    assert!(json["messages"][0]["tool_call_id"].is_null());
    assert!(json["messages"][0]["tool_calls"][0]["summary"].is_null());
}

#[test]
fn a_turn_handle_carries_both_ids() {
    let json = serde_json::to_value(TurnHandle {
        session_id: "s".to_owned(),
        turn_id: "t".to_owned(),
    })
    .expect("TurnHandle serializes");

    assert_eq!(json["session_id"], "s");
    assert_eq!(json["turn_id"], "t");
}
