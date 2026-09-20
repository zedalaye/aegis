use super::gemini::*;
use super::*;

fn tool_schema(name: &str) -> Value {
    json!({
        "type": "function",
        "function": { "name": name, "description": "", "parameters": { "type": "object" } },
    })
}

fn request(messages: Vec<WireMessage>, tools: Vec<Value>) -> ModelRequest {
    ModelRequest {
        model: "claude-sonnet-5".to_owned(),
        messages,
        tools,
    }
}

#[test]
fn a_turn_asks_for_three_cache_breakpoints() {
    let chat = to_chat_request(
        &request(
            vec![
                WireMessage::System {
                    content: "The standing instructions.".to_owned(),
                },
                WireMessage::User {
                    content: "Read the PDF.".to_owned(),
                    images: Vec::new(),
                },
            ],
            vec![tool_schema("fs_read"), tool_schema("shell_exec")],
        ),
        "claude-sonnet-5",
        None,
        false,
    )
    .expect("the request converts");

    // Tools, system, and the newest message: everything before each is a
    // prefix the next round can be served out of the cache.
    assert!(chat.system_cache, "the system prompt is a breakpoint");

    let tools = chat.tools.as_ref().expect("the tools are carried");
    assert!(
        !tools[0].cache && tools[1].cache,
        "the mark goes on the last tool, which covers the whole array"
    );

    assert!(chat.messages[0].cache, "the newest message is a breakpoint");
}

#[test]
fn the_breakpoint_skips_a_tool_result_for_the_turn_that_asked_for_it() {
    // motosan serializes `Role::Tool` without consulting `cache`, so a
    // mark there would be a breakpoint that silently is not one.
    let chat = to_chat_request(
        &request(
            vec![
                WireMessage::User {
                    content: "Read the PDF.".to_owned(),
                    images: Vec::new(),
                },
                WireMessage::Assistant {
                    content: None,
                    tool_calls: vec![WireToolCall::new("call-1", "fs_read", "{}")],
                },
                WireMessage::Tool {
                    tool_call_id: "call-1".to_owned(),
                    content: "{\"ok\":true}".to_owned(),
                    images: Vec::new(),
                },
            ],
            Vec::new(),
        ),
        "claude-sonnet-5",
        None,
        false,
    )
    .expect("the request converts");

    let marked: Vec<usize> = chat
        .messages
        .iter()
        .enumerate()
        .filter(|(_, message)| message.cache)
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        marked,
        vec![1],
        "the assistant turn carries it, not the result"
    );
}

fn reported(input: u32, read: Option<u32>, created: Option<u32>) -> motosan_ai::Usage {
    motosan_ai::Usage {
        input_tokens: input,
        output_tokens: 7,
        cache_creation_input_tokens: created,
        cache_read_input_tokens: read,
    }
}

#[test]
fn anthropics_cached_tokens_are_added_back_into_the_prompt() {
    // `input_tokens: 200` beside a 40k cache read is a 40.2k prompt that
    // was cheap to serve, not a 200-token prompt.
    let usage = to_usage(
        motosan_ai::Provider::Anthropic,
        &reported(200, Some(40_000), Some(1_500)),
    );

    assert_eq!(usage.prompt_tokens, 41_700);
    assert_eq!(usage.cache_read_tokens, 40_000);
    assert_eq!(usage.cache_creation_tokens, 1_500);
    assert_eq!(usage.total_tokens, 41_707);
}

#[test]
fn the_responses_api_counts_them_in_already_and_is_left_alone() {
    // Same numbers, other convention: adding here would report a turn as
    // more expensive the better its cache went.
    let usage = to_usage(
        motosan_ai::Provider::OpenAiChatGpt,
        &reported(41_700, Some(40_000), None),
    );

    assert_eq!(usage.prompt_tokens, 41_700);
    assert_eq!(usage.cache_read_tokens, 40_000);
    assert_eq!(usage.cache_creation_tokens, 0);
}

#[test]
fn a_provider_that_says_nothing_about_caching_reports_none() {
    let usage = to_usage(motosan_ai::Provider::Anthropic, &reported(900, None, None));

    assert_eq!(usage.prompt_tokens, 900);
    assert_eq!(usage.cache_read_tokens, 0);
    assert_eq!(usage.cache_creation_tokens, 0);
}

#[test]
fn a_request_with_nothing_to_mark_is_still_a_request() {
    let chat = to_chat_request(
        &request(Vec::new(), Vec::new()),
        "claude-sonnet-5",
        None,
        false,
    )
    .expect("the request converts");

    assert!(!chat.system_cache);
    assert!(chat.tools.is_none());
    assert!(chat.messages.is_empty());
}

#[test]
fn gemini_tool_results_carry_the_function_name_not_the_opaque_id() {
    // motosan-ai's Gemini serializer puts `tool_call_id` into
    // `functionResponse.name`, and the stream assigns opaque ids
    // (`call_0`). Without this remap the second round of an `fs_write`
    // fails because Gemini has never heard of `call_0`.
    let chat = to_chat_request(
        &request(
            vec![
                WireMessage::User {
                    content: "Write it.".to_owned(),
                    images: Vec::new(),
                },
                WireMessage::Assistant {
                    content: None,
                    tool_calls: vec![WireToolCall::new("call_0", "fs_write", "{}")],
                },
                WireMessage::Tool {
                    tool_call_id: "call_0".to_owned(),
                    content: "{\"ok\":true}".to_owned(),
                    images: Vec::new(),
                },
            ],
            Vec::new(),
        ),
        "gemini-2.5-flash",
        None,
        true,
    )
    .expect("the request converts");

    assert_eq!(
        chat.messages[2].tool_call_id.as_deref(),
        Some("fs_write"),
        "the result is named for Gemini, not the stream id"
    );

    let kept = to_chat_request(
        &request(
            vec![
                WireMessage::Assistant {
                    content: None,
                    tool_calls: vec![WireToolCall::new("call_0", "fs_write", "{}")],
                },
                WireMessage::Tool {
                    tool_call_id: "call_0".to_owned(),
                    content: "{\"ok\":true}".to_owned(),
                    images: Vec::new(),
                },
            ],
            Vec::new(),
        ),
        "claude-sonnet-5",
        None,
        false,
    )
    .expect("the request converts");

    assert_eq!(
        kept.messages[1].tool_call_id.as_deref(),
        Some("call_0"),
        "Anthropic still wants the call id"
    );
}

#[test]
fn gemini_drops_json_schema_fields_its_proto_does_not_have() {
    // The live API rejects the whole request on the first unknown field.
    // Aegis tools set `additionalProperties`; MCP tools often set `$schema`;
    // handoff briefs nest the first inside `items` and `allOf`.
    let tools = vec![json!({
        "type": "function",
        "function": {
            "name": "handoff_delegate",
            "description": "",
            "parameters": {
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "properties": {
                    "briefs": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                        },
                    },
                    "review": {
                        "allOf": [{
                            "type": "object",
                            "additionalProperties": false,
                        }],
                    },
                },
                "additionalProperties": false,
            },
        },
    })];

    let gemini = to_chat_request(
        &request(Vec::new(), tools.clone()),
        "gemini-2.5-flash",
        None,
        true,
    )
    .expect("the request converts");
    let schema = &gemini.tools.as_ref().expect("tools")[0].input_schema;
    assert!(schema.get("$schema").is_none());
    assert!(schema.get("additionalProperties").is_none());
    assert!(schema["properties"]["briefs"]["items"]
        .get("additionalProperties")
        .is_none());
    assert!(schema["properties"]["review"]["allOf"][0]
        .get("additionalProperties")
        .is_none());
    assert_eq!(schema["type"], "object");

    let anthropic = to_chat_request(&request(Vec::new(), tools), "claude-sonnet-5", None, false)
        .expect("the request converts");
    let kept = &anthropic.tools.as_ref().expect("tools")[0].input_schema;
    assert_eq!(kept["additionalProperties"], false);
    assert_eq!(
        kept["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
}

fn inline() -> WireImage {
    WireImage::Inline {
        mime: "image/png".to_owned(),
        data: "iVBOR".to_owned(),
    }
}

/// A user image, then a capture answering a call, as a turn sends them.
fn with_images() -> ModelRequest {
    request(
        vec![
            WireMessage::User {
                content: "what is on my screen?".to_owned(),
                images: vec![inline()],
            },
            WireMessage::Assistant {
                content: None,
                tool_calls: vec![WireToolCall::new("call_0", "screen_capture", "{}")],
            },
            WireMessage::Tool {
                tool_call_id: "call_0".to_owned(),
                content: "{\"ok\":true}".to_owned(),
                images: vec![inline()],
            },
        ],
        Vec::new(),
    )
}

#[test]
fn anthropic_images_are_blocks_and_a_capture_follows_its_result() {
    let chat = to_chat_request(&with_images(), "claude-sonnet-5", None, false)
        .expect("the request converts");

    let user = &chat.messages[0];
    assert!(matches!(
        user.content_blocks.as_slice(),
        [
            motosan_ai::ContentBlock::Text { .. },
            motosan_ai::ContentBlock::Image { .. }
        ]
    ));

    assert!(matches!(chat.messages[2].role, motosan_ai::Role::Tool));
    assert_eq!(
        chat.messages[2].content, "{\"ok\":true}",
        "the envelope stays text"
    );
    let after = &chat.messages[3];
    assert!(matches!(after.role, motosan_ai::Role::User));
    assert!(matches!(
        after.content_blocks.as_slice(),
        [
            motosan_ai::ContentBlock::Text { text },
            motosan_ai::ContentBlock::Image { .. }
        ] if text == TOOL_IMAGES_LEAD
    ));
}

#[test]
fn gemini_images_are_inline_data() {
    let body = gemini_body(&with_images(), None);
    let contents = body["contents"].as_array().expect("contents");

    assert_eq!(
        contents[0]["parts"][1]["inlineData"]["mimeType"],
        "image/png"
    );
    assert!(contents[2]["parts"][0].get("functionResponse").is_some());
    assert_eq!(contents[3]["role"], "user");
    assert_eq!(contents[3]["parts"][0]["text"], TOOL_IMAGES_LEAD);
    assert_eq!(contents[3]["parts"][1]["inlineData"]["data"], "iVBOR");
}

#[test]
fn a_dialect_without_images_tells_the_model() {
    let mut request = with_images();
    super::super::image::refuse_all(&mut request, "no images here");
    let chat = to_chat_request(&request, "gpt-5.5", None, false).expect("converts");

    assert!(chat.messages[0].content_blocks.is_empty());
    assert!(chat.messages[0].content.contains("no images here"));
    assert_eq!(chat.messages.len(), 3, "no image turn after the result");
}

#[test]
fn gemini_echoes_a_thought_signature_on_the_function_call_part() {
    let mut call = WireToolCall::new("call_0", "fs_list", r#"{"path":"."}"#);
    call.thought_signature = Some("sig-abc".to_owned());
    let body = gemini_body(
        &request(
            vec![
                WireMessage::User {
                    content: "list".to_owned(),
                    images: Vec::new(),
                },
                WireMessage::Assistant {
                    content: None,
                    tool_calls: vec![call],
                },
                WireMessage::Tool {
                    tool_call_id: "call_0".to_owned(),
                    content: "{\"ok\":true}".to_owned(),
                    images: Vec::new(),
                },
            ],
            vec![tool_schema("fs_list")],
        ),
        None,
    );

    let part = &body["contents"][1]["parts"][0];
    assert_eq!(part["functionCall"]["name"], "fs_list");
    assert_eq!(part["thoughtSignature"], "sig-abc");
    assert_eq!(
        body["contents"][2]["parts"][0]["functionResponse"]["name"], "fs_list",
        "the result is still remapped onto the function name"
    );
    assert!(body["tools"][0]["functionDeclarations"][0]["parameters"]
        .get("additionalProperties")
        .is_none());
}

#[test]
fn gemini_omits_thought_signature_when_the_model_did_not_send_one() {
    let body = gemini_body(
        &request(
            vec![WireMessage::Assistant {
                content: None,
                tool_calls: vec![WireToolCall::new("call_0", "fs_list", "{}")],
            }],
            Vec::new(),
        ),
        None,
    );
    assert!(body["contents"][0]["parts"][0]
        .get("thoughtSignature")
        .is_none());
}
