use super::*;

use crate::agent::transcript;
use crate::agent::wire::WireMessage;

use std::path::PathBuf;

/// Collects a whole stream.
async fn drain(provider: &FakeProvider, request: ModelRequest) -> Vec<ModelEvent> {
    let mut rx = provider.stream(request);
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    events
}

/// The text of every `TextDelta`, concatenated.
fn text_of(events: &[ModelEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            ModelEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// The owner named after the trigger, and the built-in identity without
/// one. Lowering a message can change its length, so the index the trigger
/// is found at is only ever used on the string it was found in.
#[test]
fn the_delegate_trigger_names_an_owner_without_ever_slicing_mid_character() {
    let builtin = crate::store::Agent::builtin().name;

    assert_eq!(owner_named("/delegate Scribe please"), "scribe");
    assert_eq!(owner_named("please /DELEGATE Reader"), "reader");
    assert_eq!(owner_named("/delegate"), builtin);
    assert_eq!(owner_named("nothing here"), builtin);
    // `İ` lowers to two characters, so an index into the lowered copy is
    // past the end of the original by the time the trigger is reached.
    assert_eq!(owner_named("İİİ /delegate Scribe"), "scribe");
}

fn request_saying(text: &str) -> ModelRequest {
    let agent = crate::store::Agent::builtin();
    let root = PathBuf::from("/home/p/work");

    transcript::build(
        FAKE_MODEL,
        &transcript::Context {
            agent: &agent,
            workspace: Some(&root),
            exec_host: None,
            memories: None,
            skills: None,
            world: None,
            shared: None,
            compacted: None,
            unattended: false,
        },
        &[crate::store::Message::user(text)],
        crate::tools::schemas(),
    )
}

#[tokio::test]
async fn a_reply_streams_and_then_finishes() {
    let events = drain(&FakeProvider::instant(), request_saying("hello there")).await;

    assert!(events.len() > 5, "the reply is streamed, not sent whole");
    assert!(matches!(
        events.last(),
        Some(ModelEvent::Finish {
            reason: StopReason::Stop,
            usage: Some(_)
        })
    ));
}

/// The reply is evidence about the request: if the transcript stopped
/// carrying the workspace or the tool schemas, the text says so.
#[tokio::test]
async fn the_reply_reports_what_the_request_carried() {
    let events = drain(&FakeProvider::instant(), request_saying("hello there")).await;
    let text = text_of(&events);

    assert!(text.contains("hello there"), "{text}");
    assert!(text.contains("/home/p/work"), "{text}");
    assert!(
        text.contains(&format!("{} tools", crate::tools::schemas().len())),
        "{text}"
    );
}

#[tokio::test]
async fn a_request_without_a_workspace_is_reported_as_such() {
    let agent = crate::store::Agent::builtin();
    let request = transcript::build(
        FAKE_MODEL,
        &transcript::Context {
            agent: &agent,
            workspace: None,
            exec_host: None,
            memories: None,
            skills: None,
            world: None,
            shared: None,
            compacted: None,
            unattended: false,
        },
        &[],
        Vec::new(),
    );
    let text = text_of(&drain(&FakeProvider::instant(), request).await);

    assert!(text.contains("no workspace"), "{text}");
}

#[tokio::test]
async fn a_script_is_replayed_verbatim_and_then_exhausts() {
    let scripted = vec![vec![
        ModelEvent::TextDelta {
            text: "one".to_owned(),
        },
        ModelEvent::Finish {
            reason: StopReason::ToolCalls,
            usage: None,
        },
    ]];
    let provider = FakeProvider::scripted(scripted.clone());

    assert_eq!(
        drain(&provider, request_saying("go")).await,
        scripted[0],
        "the first turn is exactly the script"
    );

    // The second turn has no script left, so it improvises rather than
    // returning nothing.
    let second = drain(&provider, request_saying("and again")).await;
    assert!(second.len() > 1);
    assert!(text_of(&second).contains("and again"));
}

/// Concatenating the tokens must reproduce the text exactly: the UI
/// appends deltas to a buffer and then swaps in the finalized message, and
/// the two have to agree.
#[test]
fn tokens_reassemble_into_the_original_text() {
    for text in [
        "hello there friend",
        "  leading and trailing  ",
        "line one\n\nline two",
        "one",
        "",
        "\u{201c}quoted\u{201d} and punctuated.",
    ] {
        assert_eq!(
            tokens(text).concat(),
            text,
            "round trip failed for {text:?}"
        );
    }
}

#[test]
fn tokens_break_on_words_rather_than_characters() {
    assert_eq!(tokens("a bc  d"), vec!["a ", "bc  ", "d"]);
}

/// A dropped receiver is how the turn loop says it stopped listening. The
/// provider must not keep producing into a channel nobody reads.
#[tokio::test]
async fn a_dropped_listener_stops_the_stream() {
    let provider = FakeProvider::new();
    let rx = provider.stream(request_saying("a long enough message to stream"));
    drop(rx);

    // Nothing to assert but the absence of a panic: the send fails, the
    // task returns. Yielding gives it the chance to do so under the test
    // runtime.
    tokio::task::yield_now().await;
}

/// PLAN 6, Phase 6: the fake provider asks for an `fs_write` on demand, so
/// the approval gate can be walked through without a model.
#[tokio::test]
async fn the_write_trigger_produces_a_real_fs_write_call() {
    let asked = drain(
        &FakeProvider::instant(),
        request_saying("please /write something"),
    )
    .await;

    let call = asked
        .iter()
        .find_map(|event| match event {
            ModelEvent::ToolCallDelta {
                name, args_delta, ..
            } => Some((name.clone(), args_delta.clone())),
            _ => None,
        })
        .expect("a tool call");

    assert_eq!(call.0.as_deref(), Some(crate::policy::tool::FS_WRITE));

    // The arguments have to be one valid JSON object, or the turn answers
    // the call with a parse error instead of asking anyone about it.
    let args: serde_json::Value = serde_json::from_str(&call.1).expect("valid arguments");
    assert_eq!(args["path"], WRITE_TARGET);
    assert!(args["content"].as_str().is_some_and(|c| !c.is_empty()));

    assert!(matches!(
        asked.last(),
        Some(ModelEvent::Finish {
            reason: StopReason::ToolCalls,
            ..
        })
    ));
}

/// PLAN 6, Phase 7: the fake provider asks for a `shell_exec` on demand,
/// so the shell tool can be walked through without a model.
#[tokio::test]
async fn the_run_trigger_produces_a_real_shell_exec_call() {
    let asked = drain(
        &FakeProvider::instant(),
        request_saying("please /run something"),
    )
    .await;

    let call = asked
        .iter()
        .find_map(|event| match event {
            ModelEvent::ToolCallDelta {
                name, args_delta, ..
            } => Some((name.clone(), args_delta.clone())),
            _ => None,
        })
        .expect("a tool call");

    assert_eq!(call.0.as_deref(), Some(crate::policy::tool::SHELL_EXEC));

    let args: serde_json::Value = serde_json::from_str(&call.1).expect("valid arguments");
    let program = args["program"].as_str().expect("a program");
    assert!(!program.is_empty());
    assert!(args["args"].is_array());

    // The arguments have to be a vector, not a command line: a single
    // element carrying spaces would be a program name with spaces in it,
    // and it would not resolve.
    for argument in args["args"].as_array().expect("an array") {
        assert!(argument.is_string());
    }
}

/// PLAN 6, Phase 9: the fake provider asks for a `screen_capture` on
/// demand, so the capture gate can be walked through without a model — and
/// without a capture ever reaching a provider, since there is none.
#[tokio::test]
async fn the_capture_trigger_produces_a_real_screen_capture_call() {
    let asked = drain(
        &FakeProvider::instant(),
        request_saying("please /capture the screen"),
    )
    .await;

    let call = asked
        .iter()
        .find_map(|event| match event {
            ModelEvent::ToolCallDelta {
                name, args_delta, ..
            } => Some((name.clone(), args_delta.clone())),
            _ => None,
        })
        .expect("a tool call");

    assert_eq!(call.0.as_deref(), Some(crate::policy::tool::SCREEN_CAPTURE));

    // An empty object, not an absent one: the turn parses the accumulated
    // arguments as JSON before anything is decided, and a call whose
    // arguments do not parse is answered rather than asked about.
    let args: serde_json::Value = serde_json::from_str(&call.1).expect("valid arguments");
    assert_eq!(args, serde_json::json!({}));
}

/// PLAN 7.3, Phase 14: the fake provider asks to remember something on
/// demand, so the one gate that protects nothing on the machine can still
/// be walked through without a model.
#[tokio::test]
async fn the_remember_trigger_asks_to_write_a_memory_about_the_demo() {
    let asked = drain(
        &FakeProvider::instant(),
        request_saying("please /remember this"),
    )
    .await;

    let call = asked
        .iter()
        .find_map(|event| match event {
            ModelEvent::ToolCallDelta {
                name, args_delta, ..
            } => Some((name.clone(), args_delta.clone())),
            _ => None,
        })
        .expect("a tool call");

    assert_eq!(call.0.as_deref(), Some(crate::policy::tool::MEMORY_WRITE));

    let args: serde_json::Value = serde_json::from_str(&call.1).expect("valid arguments");
    assert_eq!(args["kind"], "convention");
    assert_eq!(args["text"], REMEMBER_TEXT);
    // About the demo, never about the user. A scripted provider inventing a
    // preference would be putting words in somebody's mouth in the one
    // store that outlives every session.
    assert_eq!(args["source"], "agent/provider/fake.rs");
}

/// Phase 13: the skill trigger walks load, return, stop; each round is
/// decided from the last result.
#[tokio::test]
async fn the_skill_trigger_loads_a_runbook_and_then_closes_the_run() {
    /// The one call a round made, if it made one.
    fn call_of(events: &[ModelEvent]) -> Option<(String, String)> {
        events.iter().find_map(|event| match event {
            ModelEvent::ToolCallDelta {
                name, args_delta, ..
            } => Some((name.clone()?, args_delta.clone())),
            _ => None,
        })
    }

    let provider = FakeProvider::instant();
    let catalog = "Skills you may run.\n\n- `inbox.triage` (v1, this workspace) — sorts an \
                   item into the board.";

    // Round one: the catalog names a runbook, so it asks for that one.
    let mut request = request_saying("please /skill");
    if let Some(WireMessage::System { content }) = request.messages.first_mut() {
        content.push_str("\n\n");
        content.push_str(catalog);
    }
    let opened = call_of(&drain(&provider, request.clone()).await).expect("a call");
    assert_eq!(opened.0, crate::policy::tool::SKILL_RUN);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&opened.1).expect("arguments"),
        serde_json::json!({ "name": "inbox.triage" })
    );

    // Round two: the run is open, so it closes it — and honestly, with a
    // `blocked`, because it did not do the work.
    request.messages.push(WireMessage::Tool {
        tool_call_id: "call_1".to_owned(),
        content: r#"{"ok":true,"tool":"skill_run","meta":{"skill":"inbox.triage"}}"#.to_owned(),
        images: Vec::new(),
    });
    let closed = call_of(&drain(&provider, request.clone()).await).expect("a call");
    assert_eq!(closed.0, crate::policy::tool::SKILL_RETURN);
    let args: serde_json::Value = serde_json::from_str(&closed.1).expect("arguments");
    assert_eq!(args["status"], "blocked");
    assert!(
        args["open_questions"]
            .as_array()
            .is_some_and(|questions| !questions.is_empty()),
        "a blocked with nothing to answer would be refused by the runner: {args}"
    );

    // Round three: the run is closed, so it says so and stops.
    request.messages.push(WireMessage::Tool {
        tool_call_id: "call_2".to_owned(),
        content: r#"{"ok":true,"tool":"skill_return","meta":{"skill":"inbox.triage"}}"#.to_owned(),
        images: Vec::new(),
    });
    let finished = drain(&provider, request).await;
    assert!(call_of(&finished).is_none(), "the loop ends in a word");
    assert!(text_of(&finished).contains("audit log"), "{:?}", finished);
}

/// An identity granted no skills has no catalog in its system message, and
/// the trigger says so rather than inventing a name to call.
#[tokio::test]
async fn the_skill_trigger_with_no_catalog_asks_for_nothing() {
    let events = drain(&FakeProvider::instant(), request_saying("/skill please")).await;

    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ModelEvent::ToolCallDelta { .. })),
        "nothing to run means no call: {events:?}"
    );
    assert!(text_of(&events).contains("granted no skills"));
}

/// The trigger fires once per turn, not once per round. A provider that
/// asked again after the tool answered would burn all eight rounds on the
/// same file and look exactly like a gate that is not holding.
#[tokio::test]
async fn the_trigger_does_not_fire_again_once_the_call_is_answered() {
    let mut request = request_saying("please /write something");
    request.messages.push(WireMessage::Tool {
        tool_call_id: "call_1".to_owned(),
        content: r#"{"ok":true}"#.to_owned(),
        images: Vec::new(),
    });

    let events = drain(&FakeProvider::instant(), request).await;

    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ModelEvent::ToolCallDelta { .. })),
        "the second round explains itself instead of asking again"
    );
}

#[tokio::test]
async fn an_ordinary_message_never_reaches_for_a_tool() {
    let events = drain(&FakeProvider::instant(), request_saying("hello there")).await;

    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ModelEvent::ToolCallDelta { .. })),
        "the fake model only touches the disk when asked to by name"
    );
}

#[tokio::test]
async fn the_model_id_is_what_the_turn_reports() {
    let provider = FakeProvider::new();
    assert_eq!(provider.model(), FAKE_MODEL);

    let request = request_saying("x");
    assert_eq!(request.model, FAKE_MODEL);
    assert!(matches!(request.messages[0], WireMessage::System { .. }));
}

/// The arguments of the one `fs_write` a set of events asks for.
fn write_arguments(events: &[ModelEvent]) -> String {
    events
        .iter()
        .find_map(|event| match event {
            ModelEvent::ToolCallDelta {
                name, args_delta, ..
            } if name.as_deref() == Some(crate::policy::tool::FS_WRITE) => Some(args_delta.clone()),
            _ => None,
        })
        .expect("the run asks to write")
}

/// The transcript of a run that parked its write and was then answered: the
/// call, the `E_PARKED` envelope, the sentence a person's answer opens with,
/// and the runbook loaded again.
fn resumed(parked: &str, answer: crate::approval::Decision) -> ModelRequest {
    use crate::agent::wire::WireToolCall;
    use crate::policy::tool;

    let ask = crate::store::ParkedAsk {
        id: "park-1".to_owned(),
        project_id: "p1".to_owned(),
        session_id: "s1".to_owned(),
        agent_id: "a1".to_owned(),
        routine_id: "r1".to_owned(),
        routine_name: "Morning watch".to_owned(),
        skill: "watch.digest".to_owned(),
        turn_id: "t1".to_owned(),
        call_id: "c1".to_owned(),
        tool: tool::FS_WRITE.to_owned(),
        fingerprint: "fs_write:abc".to_owned(),
        cause: crate::store::ParkCause::Unattended,
        risk: crate::policy::Risk::Medium,
        title: "Write file".to_owned(),
        summary: "watch.digest.md (32 B, new file)".to_owned(),
        detail: crate::policy::ApprovalDetail::FsWrite {
            path: "/w/.aegis/status/watch.digest.md".to_owned(),
            bytes: 32,
            exists: false,
            preview: None,
            applies: None,
        },
        reason: "this creates a file in the workspace".to_owned(),
        grant: Some(crate::policy::Grant::FsWrite),
        scope_label: crate::policy::Grant::FsWrite.standing_label(),
        parked_at: "2026-09-20T07:00:00.000Z".to_owned(),
        expires_at: "2026-09-27T07:00:00.000Z".to_owned(),
    };

    ModelRequest {
        model: FAKE_MODEL.to_owned(),
        tools: Vec::new(),
        messages: vec![
            WireMessage::user("This is a scheduled run. Run `skill:watch.digest` now."),
            WireMessage::Assistant {
                content: None,
                tool_calls: vec![WireToolCall::new("c1", tool::FS_WRITE, parked)],
            },
            WireMessage::tool(
                "c1",
                r#"{"ok":false,"tool":"fs_write","error":{"code":"E_PARKED"}}"#,
            ),
            WireMessage::user(crate::park::resumption(&ask, answer)),
            WireMessage::Assistant {
                content: None,
                tool_calls: vec![WireToolCall::new("c2", tool::SKILL_RUN, "{}")],
            },
            WireMessage::tool("c2", r#"{"ok":true,"tool":"skill_run"}"#),
        ],
    }
}

/// PLAN 7.22: an answer matches one exact call, so a run picked up after one
/// makes that call again *unchanged*. Composing a second, slightly different
/// one — this runbook's write carries a timestamp — would be a new question,
/// and the one-shot would not cover it.
#[test]
fn a_resumed_run_makes_the_parked_call_again_unchanged() {
    let parked = r##"{"path":".aegis/status/watch.digest.md","content":"# watch.digest\n","create_dirs":true}"##;

    let events = improvise(&resumed(parked, crate::approval::Decision::AllowOnce));

    assert_eq!(
        write_arguments(&events),
        parked,
        "the call a person read is the call that runs"
    );
}

/// A refusal is not a puzzle: the run closes instead of asking the same thing
/// a second way.
#[test]
fn a_refused_answer_closes_the_run_rather_than_writing() {
    let parked = r##"{"path":".aegis/status/watch.digest.md","content":"# watch.digest\n","create_dirs":true}"##;

    let events = improvise(&resumed(parked, crate::approval::Decision::Deny));

    let returned = events
        .iter()
        .find_map(|event| match event {
            ModelEvent::ToolCallDelta {
                name, args_delta, ..
            } if name.as_deref() == Some(crate::policy::tool::SKILL_RETURN) => {
                Some(args_delta.clone())
            }
            _ => None,
        })
        .expect("the run closes itself");
    assert!(returned.contains("blocked"), "{returned}");
    assert!(
        !events.iter().any(|event| matches!(
            event,
            ModelEvent::ToolCallDelta { name, .. }
                if name.as_deref() == Some(crate::policy::tool::FS_WRITE)
        )),
        "a refused call is not made again"
    );
}

/// The same rule in a session someone opened: a dialog that expired and was
/// then allowed is the call a person read, made again as it was written
/// (PLAN 7.22). There is no runbook here, so this is the whole turn.
#[test]
fn an_answered_session_makes_the_call_again_and_then_reports() {
    use crate::agent::wire::WireToolCall;
    use crate::policy::tool;

    let parked = r##"{"path":"notes.md","content":"# notes\n"}"##;
    let answered = |answers: Vec<WireMessage>| {
        let mut messages = vec![
            WireMessage::user("/write a file for me"),
            WireMessage::Assistant {
                content: None,
                tool_calls: vec![WireToolCall::new("c1", tool::FS_WRITE, parked)],
            },
            WireMessage::tool(
                "c1",
                r#"{"ok":false,"tool":"fs_write","error":{"code":"E_PARKED"}}"#,
            ),
            WireMessage::user(
                "A person has answered the `fs_write` call this run parked: it is allowed this \
                 once. Make that call again now, unchanged.",
            ),
        ];
        messages.extend(answers);
        ModelRequest {
            model: FAKE_MODEL.to_owned(),
            tools: Vec::new(),
            messages,
        }
    };

    let events = improvise(&answered(Vec::new()));
    assert_eq!(
        events
            .iter()
            .find_map(|event| match event {
                ModelEvent::ToolCallDelta {
                    name, args_delta, ..
                } if name.as_deref() == Some(tool::FS_WRITE) => Some(args_delta.clone()),
                _ => None,
            })
            .expect("the allowed call is made"),
        parked
    );

    // And once it has run, the turn says so rather than asking again.
    let after = answered(vec![
        WireMessage::Assistant {
            content: None,
            tool_calls: vec![WireToolCall::new("c2", tool::FS_WRITE, parked)],
        },
        WireMessage::tool("c2", r#"{"ok":true,"tool":"fs_write"}"#),
    ]);
    assert!(
        !improvise(&after)
            .iter()
            .any(|event| matches!(event, ModelEvent::ToolCallDelta { .. })),
        "a call a person allowed once is made once"
    );
}
