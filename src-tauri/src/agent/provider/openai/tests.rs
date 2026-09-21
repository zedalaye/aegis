use super::*;

/// Every payload the decoder produces from one byte slice.
fn decode(chunks: &[&str]) -> Vec<String> {
    let mut decoder = SseDecoder::default();
    let mut out = Vec::new();
    for chunk in chunks {
        out.extend(decoder.push(chunk.as_bytes()).expect("within the cap"));
    }
    out
}

#[test]
fn a_frame_is_dispatched_by_its_blank_line() {
    assert_eq!(
        decode(&["data: one\n\ndata: two\n\n"]),
        vec!["one".to_owned(), "two".to_owned()]
    );
}

/// The one property the whole decoder exists for: the network decides
/// where a chunk ends, and it is never where the protocol does.
#[test]
fn a_payload_split_across_chunks_is_reassembled() {
    assert_eq!(
        decode(&["da", "ta: {\"a\":", "1}", "\n", "\n"]),
        vec![r#"{"a":1}"#.to_owned()]
    );
}

#[test]
fn crlf_and_lone_cr_line_endings_both_work() {
    assert_eq!(decode(&["data: one\r\n\r\n"]), vec!["one".to_owned()]);
    assert_eq!(
        decode(&["data: two\r\rdata: three\r\r\n"]),
        vec!["two".to_owned(), "three".to_owned()]
    );
}

/// A CRLF split across chunks does not produce a spurious empty line.
#[test]
fn a_trailing_carriage_return_waits_for_what_follows_it() {
    let mut decoder = SseDecoder::default();

    assert!(decoder
        .push(b"data: one\r\n\r")
        .expect("within the cap")
        .is_empty());
    assert_eq!(
        decoder.push(b"\n").expect("within the cap"),
        vec!["one".to_owned()]
    );
}

/// A CRLF split down the middle must not look like two line endings — the
/// second of which would dispatch the event early and cut the payload off.
#[test]
fn a_crlf_split_between_chunks_is_one_line_ending() {
    assert_eq!(
        decode(&["data: one\r", "\ndata: two\r\n\r\n"]),
        vec!["one\ntwo"]
    );
}

/// A character split across chunks arrives whole, because a partial UTF-8
/// sequence contains no newline byte and simply waits.
#[test]
fn a_multi_byte_character_split_across_chunks_survives() {
    let text = "é😀";
    let bytes = format!("data: {text}\n\n").into_bytes();

    let mut decoder = SseDecoder::default();
    let mut out = Vec::new();
    for byte in bytes {
        out.extend(decoder.push(&[byte]).expect("within the cap"));
    }

    assert_eq!(out, vec![text.to_owned()]);
}

#[test]
fn comments_and_unknown_fields_are_ignored() {
    assert_eq!(
        decode(&[": keep-alive\n\nevent: message\nid: 7\ndata: real\n\n"]),
        vec!["real".to_owned()],
        "a comment dispatches nothing, and only `data` is kept"
    );
}

/// The grammar joins several `data:` lines with newlines. No server in the
/// chat API does this, which is exactly why it is worth pinning.
#[test]
fn several_data_lines_join_with_newlines() {
    assert_eq!(decode(&["data: a\ndata: b\n\n"]), vec!["a\nb".to_owned()]);
}

#[test]
fn a_data_line_without_a_space_after_the_colon_keeps_its_value() {
    assert_eq!(decode(&["data:tight\n\n"]), vec!["tight".to_owned()]);
}

#[test]
fn an_endless_line_is_refused_rather_than_buffered_forever() {
    let mut decoder = SseDecoder::default();
    let flood = vec![b'x'; MAX_PENDING_BYTES / 2 + 1];

    assert!(decoder.push(&flood).expect("the first fits").is_empty());
    assert!(decoder.push(&flood).is_err(), "the second must be refused");
}

// -- frame mapping ------------------------------------------------------

/// Absorbs frames and returns the events plus the finish, the way `run`
/// does.
fn events(frames: &[Value], done: bool) -> Vec<ModelEvent> {
    let mut state = StreamState::default();
    let mut out = Vec::new();

    for frame in frames {
        out.extend(state.absorb(&frame.to_string()));
    }
    state.done = done || state.done;
    out.extend(state.finish());
    out
}

#[test]
fn text_deltas_become_text_deltas() {
    let out = events(
        &[
            json!({ "choices": [{ "index": 0, "delta": { "content": "Hel" } }] }),
            json!({ "choices": [{ "index": 0, "delta": { "content": "lo" } }] }),
            json!({ "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }] }),
        ],
        true,
    );

    assert_eq!(
        out,
        vec![
            ModelEvent::TextDelta {
                text: "Hel".to_owned()
            },
            ModelEvent::TextDelta {
                text: "lo".to_owned()
            },
            ModelEvent::Finish {
                reason: StopReason::Stop,
                usage: None,
            },
        ]
    );
}

/// The deferred finish, which is the reason `StreamState` exists: the
/// usage chunk arrives after the one that said why the model stopped, and
/// the turn loop stops reading at the first finish it is handed.
#[test]
fn usage_sent_after_the_finish_reason_still_reaches_the_turn() {
    let out = events(
        &[
            json!({ "choices": [{ "index": 0, "delta": { "content": "hi" } }] }),
            json!({ "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }] }),
            json!({
                "choices": [],
                "usage": { "prompt_tokens": 11, "completion_tokens": 4, "total_tokens": 15 }
            }),
        ],
        true,
    );

    assert_eq!(
        out.last(),
        Some(&ModelEvent::Finish {
            reason: StopReason::Stop,
            usage: Some(Usage {
                prompt_tokens: 11,
                completion_tokens: 4,
                total_tokens: 15,
                ..Usage::default()
            }),
        })
    );
    assert_eq!(
        out.iter()
            .filter(|event| matches!(event, ModelEvent::Finish { .. }))
            .count(),
        1,
        "exactly one finish per response"
    );
}

#[test]
fn a_total_the_server_omitted_is_added_up() {
    let out = events(
        &[json!({
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 7, "completion_tokens": 3 }
        })],
        true,
    );

    assert!(matches!(
        out.last(),
        Some(ModelEvent::Finish {
            usage: Some(Usage {
                total_tokens: 10,
                ..
            }),
            ..
        })
    ));
}

#[test]
fn tool_call_fragments_carry_their_index_and_are_named_once() {
    let out = events(
        &[
            json!({ "choices": [{ "delta": { "tool_calls": [{
                "index": 0, "id": "call_a", "type": "function",
                "function": { "name": "fs_read", "arguments": "" }
            }] } }] }),
            json!({ "choices": [{ "delta": { "tool_calls": [{
                "index": 0, "id": "", "function": { "arguments": "{\"path\":" }
            }] } }] }),
            json!({ "choices": [{ "delta": {}, "finish_reason": "tool_calls" }] }),
        ],
        true,
    );

    assert_eq!(
        out[0],
        ModelEvent::ToolCallDelta {
            index: 0,
            id: Some("call_a".to_owned()),
            name: Some("fs_read".to_owned()),
            args_delta: String::new(),
            thought_signature: None,
        }
    );
    assert_eq!(
        out[1],
        ModelEvent::ToolCallDelta {
            index: 0,
            id: None,
            name: None,
            args_delta: r#"{"path":"#.to_owned(),
            thought_signature: None,
        },
        "an empty id must not overwrite the real one"
    );
    assert!(matches!(
        out.last(),
        Some(ModelEvent::Finish {
            reason: StopReason::ToolCalls,
            ..
        })
    ));
}

/// Some servers end the stream with `[DONE]` and never send a
/// `finish_reason`. What the response did is still visible from what it
/// sent.
#[test]
fn a_stream_that_only_says_done_still_finishes() {
    assert!(matches!(
        events(
            &[json!({ "choices": [{ "delta": { "content": "hi" } }] })],
            true
        )
        .last(),
        Some(ModelEvent::Finish {
            reason: StopReason::Stop,
            ..
        })
    ));

    assert!(matches!(
        events(
            &[json!({ "choices": [{ "delta": { "tool_calls": [{
                "index": 0, "id": "c", "function": { "name": "fs_list", "arguments": "{}" }
            }] } }] })],
            true
        )
        .last(),
        Some(ModelEvent::Finish {
            reason: StopReason::ToolCalls,
            ..
        })
    ));
}

/// A body that stopped without `[DONE]` and without a reason is a
/// truncated reply. The provider says nothing so the turn loop can report
/// it, rather than inventing a stop reason no server sent.
#[test]
fn a_truncated_stream_produces_no_finish() {
    let out = events(
        &[json!({ "choices": [{ "delta": { "content": "hi" } }] })],
        false,
    );

    assert_eq!(
        out,
        vec![ModelEvent::TextDelta {
            text: "hi".to_owned()
        }]
    );
}

#[test]
fn an_error_frame_inside_a_200_ends_the_stream() {
    let out = events(
        &[json!({ "error": { "message": "context length exceeded", "type": "invalid_request" } })],
        false,
    );

    let ModelEvent::Error { code, message, .. } = &out[0] else {
        panic!("expected an error, got {out:?}");
    };
    assert_eq!(code, "E_PROVIDER_HTTP");
    assert!(message.contains("context length exceeded"), "{message}");
}

#[test]
fn a_frame_that_is_not_json_is_skipped_rather_than_fatal() {
    let mut state = StreamState::default();
    assert!(state.absorb("not json at all").is_empty());
    assert!(!state.done, "the reply is still arriving");
}

#[test]
fn an_unfamiliar_finish_reason_still_finishes_the_reply() {
    assert!(matches!(
        events(
            &[json!({ "choices": [{ "delta": {}, "finish_reason": "content_filter" }] })],
            true
        )
        .last(),
        Some(ModelEvent::Finish {
            reason: StopReason::Stop,
            ..
        })
    ));
}

// -- endpoints and headers ---------------------------------------------

/// The path is appended, never joined: `Url::join` is relative to the
/// document and would drop the `/v1` that every server needs.
#[test]
fn the_endpoint_keeps_the_whole_base_path() {
    assert_eq!(
        endpoint("https://api.openai.com/v1", CHAT_PATH)
            .expect("a URL")
            .as_str(),
        "https://api.openai.com/v1/chat/completions"
    );
    assert_eq!(
        endpoint("http://127.0.0.1:11434/v1/", CHAT_PATH)
            .expect("a URL")
            .as_str(),
        "http://127.0.0.1:11434/v1/chat/completions"
    );
}

#[test]
fn a_key_is_sent_as_a_bearer_token_and_marked_unprintable() {
    let key = ApiKey::new("sk-test-1234").expect("a key");
    let header = authorization(&key).expect("a header");

    assert!(header.is_sensitive(), "a printable key ends up in a log");
    assert_eq!(
        format!("{header:?}"),
        "Sensitive",
        "the header must not render its value"
    );
}

/// A key with a newline or a control character in it cannot go in a
/// header. The failure names the shape of the problem and never the key.
#[test]
fn a_key_that_cannot_be_a_header_is_refused_without_quoting_it() {
    let key = ApiKey::new("sk-bad\u{7f}value").expect("a key");
    let reason = authorization(&key).expect_err("refused");

    assert!(reason.contains("cannot be sent in a header"), "{reason}");
    assert!(!reason.contains("sk-bad"), "{reason}");
}

// -- provider construction ---------------------------------------------

fn settings() -> ProviderSettings {
    ProviderSettings {
        auth_kind: crate::store::AuthKind::ApiKey,
        base_url: "https://api.example.test/v1".to_owned(),
        model: "some-model".to_owned(),
        max_output_tokens: None,
        prices: Vec::new(),
    }
}

/// Construction never fails; a missing key is reported as the stream's
/// only event, so the user's message is still recorded and the failure
/// still lands in the transcript.
#[tokio::test]
async fn a_provider_with_no_key_answers_with_e_no_api_key() {
    let provider = OpenAiProvider::new(Some(Client::new()), &settings(), None);
    assert_eq!(provider.model(), "some-model");

    let mut stream = provider.stream(ModelRequest {
        model: "some-model".to_owned(),
        messages: Vec::new(),
        tools: Vec::new(),
    });

    let ModelEvent::Error { code, message, .. } = stream.recv().await.expect("one event") else {
        panic!("expected an error event");
    };
    assert_eq!(code, "E_NO_API_KEY");
    assert!(message.contains(crate::secrets::ENV_API_KEY), "{message}");
    assert!(stream.recv().await.is_none(), "and nothing after it");
}

#[tokio::test]
async fn a_provider_with_an_unusable_base_url_says_so() {
    let broken = ProviderSettings {
        auth_kind: crate::store::AuthKind::ApiKey,
        base_url: "not a url".to_owned(),
        model: "m".to_owned(),
        max_output_tokens: None,
        prices: Vec::new(),
    };
    let provider = OpenAiProvider::new(Some(Client::new()), &broken, ApiKey::new("sk-test-1234"));

    let mut stream = provider.stream(ModelRequest {
        model: "m".to_owned(),
        messages: Vec::new(),
        tools: Vec::new(),
    });

    let ModelEvent::Error { code, message, .. } = stream.recv().await.expect("one event") else {
        panic!("expected an error event");
    };
    assert_eq!(code, "E_PROVIDER_HTTP");
    assert!(message.contains("Settings"), "{message}");
}

// -- probe --------------------------------------------------------------

#[tokio::test]
async fn an_unconfigured_probe_explains_the_scripted_provider() {
    let probe = probe(Some(&Client::new()), &ProviderSettings::default(), None).await;

    assert!(!probe.ok);
    assert_eq!(probe.status, None);
    assert!(
        probe.message.contains("scripted provider"),
        "{}",
        probe.message
    );
}

/// The probe sends a real completion, so it needs a model to name. Saying
/// so beats sending `"model": ""` and relaying whatever the server makes
/// of it.
#[tokio::test]
async fn a_probe_with_no_model_says_so_without_sending_anything() {
    let half_configured = ProviderSettings {
        auth_kind: crate::store::AuthKind::ApiKey,
        base_url: "https://api.example.test/v1".to_owned(),
        model: String::new(),
        max_output_tokens: None,
        prices: Vec::new(),
    };
    let probe = probe(Some(&Client::new()), &half_configured, None).await;

    assert!(!probe.ok);
    assert_eq!(probe.status, None, "nothing was sent");
    assert!(
        probe.message.contains("No model is set"),
        "{}",
        probe.message
    );
}

#[test]
fn a_long_error_body_is_shortened_rather_than_shown_whole() {
    let long = "x".repeat(ERROR_BODY_CHARS * 2);
    let shortened = truncate(&long, ERROR_BODY_CHARS);

    assert_eq!(shortened.chars().count(), ERROR_BODY_CHARS + 1);
    assert!(shortened.ends_with('…'));
}

#[test]
fn a_responses_stream_keeps_text_calls_and_cached_usage() {
    let mut stream = StreamState::default();

    let text = stream.absorb(r#"{"type":"response.output_text.delta","delta":"Hi"}"#);
    assert!(matches!(text.as_slice(), [ModelEvent::TextDelta { text }] if text == "Hi"));

    let added = stream.absorb(
        r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"call_1","name":"fs_read","arguments":""}}"#,
    );
    assert!(matches!(
        added.as_slice(),
        [ModelEvent::ToolCallDelta { index: 0, id: Some(id), name: Some(name), args_delta, .. }]
            if id == "call_1" && name == "fs_read" && args_delta.is_empty()
    ));

    let args = stream.absorb(
        r#"{"type":"response.function_call_arguments.delta","output_index":0,"delta":"{}"}"#,
    );
    assert!(matches!(
        args.as_slice(),
        [ModelEvent::ToolCallDelta { args_delta, .. }] if args_delta == "{}"
    ));

    let completed = stream.absorb(
        r#"{"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":10,"output_tokens":4,"total_tokens":14,"input_tokens_details":{"cached_tokens":7}}}}"#,
    );
    assert!(completed.is_empty());

    let ModelEvent::Finish { reason, usage } = stream.finish().expect("a finish") else {
        panic!("expected finish");
    };
    assert_eq!(reason, StopReason::ToolCalls);
    let usage = usage.expect("usage");
    assert_eq!(usage.prompt_tokens, 10);
    assert_eq!(usage.completion_tokens, 4);
    assert_eq!(usage.cache_read_tokens, 7);
    assert_eq!(usage.total_tokens, 14);
}

#[test]
fn an_error_body_is_reduced_to_the_servers_own_message() {
    assert_eq!(
        error_message(&json!({ "message": "invalid model", "code": 400 })),
        "invalid model"
    );
    assert_eq!(
        error_message(&json!("plain text")),
        "\"plain text\"",
        "a shape with no message is quoted rather than dropped"
    );
}
