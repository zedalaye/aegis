//! Gemini through the Generative Language API: the streaming loop, the request
//! body and the schema keywords its proto refuses.

use super::*;

pub(super) fn gemini_provider(
    access_token: &str,
    model: &str,
    base_url: &str,
) -> motosan_ai::providers::gemini::GeminiProvider {
    let origin = crate::agent::provider::catalog::effective_base_url(AuthKind::Gemini, base_url);
    let origin = if origin.is_empty() {
        None
    } else {
        Some(origin)
    };
    motosan_ai::providers::gemini::GeminiProvider::new(access_token, Some(model.to_owned()), origin)
}

/// Gemini's own HTTP dialect, spoken here rather than through motosan-ai.
///
/// motosan 0.27.1 drops `thoughtSignature` on the way in and never puts it
/// back on a `functionCall` part. Gemini 3 rejects the next round without it
/// (`Function call is missing a thought_signature`). We own both sides so
/// the signature survives the transcript.
pub(super) async fn stream_gemini(
    access_token: &str,
    settings: &ProviderSettings,
    request: ModelRequest,
    http: Option<reqwest::Client>,
    tx: mpsc::Sender<ModelEvent>,
) {
    let body = gemini_body(&request, settings.max_output_tokens);
    let base =
        crate::agent::provider::catalog::effective_base_url(AuthKind::Gemini, &settings.base_url);
    let url = format!(
        "{}/models/{}:streamGenerateContent?alt=sse",
        base, settings.model
    );

    let client = match http {
        Some(client) => client,
        None => match reqwest::Client::builder().build() {
            Ok(client) => client,
            Err(err) => {
                fail(
                    &tx,
                    ErrorCode::ProviderHttp,
                    format!("No HTTP client ({err})."),
                    false,
                )
                .await;
                return;
            }
        },
    };

    let key = match reqwest::header::HeaderValue::from_str(access_token) {
        Ok(mut header) => {
            header.set_sensitive(true);
            header
        }
        Err(_) => {
            fail(
                &tx,
                ErrorCode::ProviderHttp,
                "The API key cannot be sent in a header.".to_owned(),
                false,
            )
            .await;
            return;
        }
    };

    let sending = client
        .post(&url)
        .header("x-goog-api-key", key)
        .header(
            reqwest::header::ACCEPT,
            reqwest::header::HeaderValue::from_static("text/event-stream"),
        )
        .header(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        )
        .json(&body)
        .send();

    let mut response = tokio::select! {
        biased;
        () = tx.closed() => return,
        sent = sending => match sent {
            Ok(response) => response,
            Err(err) => {
                fail(
                    &tx,
                    ErrorCode::ProviderHttp,
                    format!("The Gemini request failed ({err})."),
                    true,
                )
                .await;
                return;
            }
        },
    };

    let status = response.status();
    if !status.is_success() {
        let detail = response.text().await.unwrap_or_default();
        let retryable = status.is_server_error() || status.as_u16() == 429;
        fail(
            &tx,
            ErrorCode::ProviderHttp,
            if detail.is_empty() {
                format!("The Gemini endpoint answered {status}.")
            } else {
                format!("The Gemini endpoint answered {status}: {detail}")
            },
            retryable,
        )
        .await;
        return;
    }

    let mut decoder = crate::agent::provider::openai::SseDecoder::default();
    let mut usage: Option<Usage> = None;
    let mut saw_tool_call = false;
    let mut reason = None;
    let mut next_index = 0u32;
    let mut pending_thought: Option<String> = None;

    loop {
        let chunk = tokio::select! {
            biased;
            () = tx.closed() => return,
            chunk = response.chunk() => chunk,
        };

        let bytes = match chunk {
            Ok(Some(bytes)) => bytes,
            Ok(None) => break,
            Err(err) => {
                fail(
                    &tx,
                    ErrorCode::ProviderHttp,
                    format!("The Gemini stream broke mid-reply ({err})."),
                    true,
                )
                .await;
                return;
            }
        };

        let payloads = match decoder.push(&bytes) {
            Ok(payloads) => payloads,
            Err(message) => {
                fail(&tx, ErrorCode::ProviderParse, message, false).await;
                return;
            }
        };

        for payload in payloads {
            if payload.trim() == "[DONE]" || payload.trim().is_empty() {
                continue;
            }
            let frame: Value = match serde_json::from_str(&payload) {
                Ok(frame) => frame,
                Err(_) => continue,
            };

            if let Some(error) = frame.get("error") {
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Gemini reported an error.");
                fail(&tx, ErrorCode::ProviderHttp, message.to_owned(), false).await;
                return;
            }

            if let Some(meta) = frame.get("usageMetadata") {
                let input = meta
                    .get("promptTokenCount")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let output = meta
                    .get("candidatesTokenCount")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let round = Usage {
                    prompt_tokens: input,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                    completion_tokens: output,
                    total_tokens: input.saturating_add(output),
                };
                match &mut usage {
                    Some(spent) => spent.add(round),
                    None => usage = Some(round),
                }
            }

            let Some(candidate) = frame.get("candidates").and_then(|c| c.get(0)) else {
                continue;
            };

            let parts = candidate
                .get("content")
                .and_then(|c| c.get("parts"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();

            for part in &parts {
                if let Some(sig) = thought_signature_of(part) {
                    pending_thought = Some(sig);
                }

                let thought = part
                    .get("thought")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    if !thought
                        && !text.is_empty()
                        && tx
                            .send(ModelEvent::TextDelta {
                                text: text.to_owned(),
                            })
                            .await
                            .is_err()
                    {
                        return;
                    }
                }

                if let Some(fc) = part.get("functionCall") {
                    saw_tool_call = true;
                    let name = fc
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    let args = fc.get("args").cloned().unwrap_or_else(|| json!({}));
                    let args_str = args.to_string();
                    let index = next_index;
                    next_index = next_index.saturating_add(1);
                    let id = format!("call_{index}");
                    let signature = thought_signature_of(part).or_else(|| pending_thought.take());
                    if tx
                        .send(ModelEvent::ToolCallDelta {
                            index,
                            id: Some(id),
                            name: Some(name),
                            args_delta: args_str,
                            thought_signature: signature,
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }

            if let Some(finish) = candidate.get("finishReason").and_then(Value::as_str) {
                reason = Some(match finish {
                    "STOP" if saw_tool_call => StopReason::ToolCalls,
                    "STOP" => StopReason::Stop,
                    "MAX_TOKENS" => StopReason::Length,
                    _ if saw_tool_call => StopReason::ToolCalls,
                    _ => StopReason::Stop,
                });
            }
        }
    }

    let reason = reason.unwrap_or(if saw_tool_call {
        StopReason::ToolCalls
    } else {
        StopReason::Stop
    });
    let _ = tx.send(ModelEvent::Finish { reason, usage }).await;
}

pub(super) fn thought_signature_of(part: &Value) -> Option<String> {
    part.get("thoughtSignature")
        .or_else(|| part.get("thought_signature"))
        .or_else(|| {
            part.get("functionCall").and_then(|fc| {
                fc.get("thoughtSignature")
                    .or_else(|| fc.get("thought_signature"))
            })
        })
        .and_then(Value::as_str)
        .filter(|sig| !sig.is_empty())
        .map(str::to_owned)
}

/// Gemini `generateContent` body, including thought signatures and the
/// schema/tool-name dialect the live API actually accepts.
pub(super) fn gemini_body(request: &ModelRequest, max_tokens: Option<u32>) -> Value {
    let mut contents: Vec<Value> = Vec::new();
    let mut system: Vec<String> = Vec::new();
    let mut names: HashMap<String, String> = HashMap::new();

    // A round's capture follows its function responses as a user turn, like
    // the other dialects (PLAN 7.20).
    let mut pending: Vec<Value> = Vec::new();

    for message in &request.messages {
        if !matches!(message, WireMessage::Tool { .. }) {
            flush_tool_images(&mut contents, &mut pending);
        }
        match message {
            WireMessage::System { content } => system.push(content.clone()),
            WireMessage::User { content, images } => {
                let text = with_notes(content, images);
                let mut parts = Vec::new();
                if !text.is_empty() || images.is_empty() {
                    parts.push(json!({"text": text}));
                }
                parts.extend(images.iter().filter_map(gemini_image));
                if parts.is_empty() {
                    parts.push(json!({"text": ""}));
                }
                contents.push(json!({"role": "user", "parts": parts}));
            }
            WireMessage::Assistant {
                content,
                tool_calls,
            } => {
                let mut parts: Vec<Value> = Vec::new();
                if let Some(text) = content {
                    if !text.is_empty() {
                        parts.push(json!({"text": text}));
                    }
                }
                for call in tool_calls {
                    names.insert(call.id.clone(), call.function.name.clone());
                    let input = serde_json::from_str(&call.function.arguments)
                        .unwrap_or_else(|_| json!({}));
                    let mut part = json!({
                        "functionCall": {
                            "name": call.function.name,
                            "args": input,
                        }
                    });
                    if let Some(sig) = call
                        .thought_signature
                        .as_deref()
                        .filter(|sig| !sig.is_empty())
                    {
                        part["thoughtSignature"] = json!(sig);
                    }
                    parts.push(part);
                }
                if parts.is_empty() {
                    parts.push(json!({"text": ""}));
                }
                contents.push(json!({"role": "model", "parts": parts}));
            }
            WireMessage::Tool {
                tool_call_id,
                content,
                images,
            } => {
                let name = names
                    .get(tool_call_id)
                    .cloned()
                    .unwrap_or_else(|| tool_call_id.clone());
                let content = with_notes(content, images);
                let response: Value =
                    serde_json::from_str(&content).unwrap_or_else(|_| json!({"result": content}));
                contents.push(json!({
                    "role": "user",
                    "parts": [{"functionResponse": {"name": name, "response": response}}]
                }));
                pending.extend(images.iter().filter_map(gemini_image));
            }
        }
    }
    flush_tool_images(&mut contents, &mut pending);

    let max_tokens = max_tokens.unwrap_or(8192);
    let mut body = json!({
        "contents": contents,
        "generationConfig": { "maxOutputTokens": max_tokens },
    });

    if !system.is_empty() {
        body["systemInstruction"] = json!({"parts": [{"text": system.join("\n\n")}]});
    }

    let tools = tools_of(&request.tools, true);
    if !tools.is_empty() {
        let declarations: Vec<Value> = tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema,
                })
            })
            .collect();
        body["tools"] = json!([{"functionDeclarations": declarations}]);
        body["toolConfig"] = json!({"functionCallingConfig": {"mode": "AUTO"}});
    }

    body
}

pub(super) fn gemini_image(image: &WireImage) -> Option<Value> {
    match image {
        WireImage::Inline { mime, data } => {
            Some(json!({"inlineData": {"mimeType": mime, "data": data}}))
        }
        WireImage::File { .. } | WireImage::Missing { .. } => None,
    }
}

pub(super) fn flush_tool_images(contents: &mut Vec<Value>, pending: &mut Vec<Value>) {
    if pending.is_empty() {
        return;
    }
    let mut parts = vec![json!({"text": TOOL_IMAGES_LEAD})];
    parts.append(pending);
    contents.push(json!({"role": "user", "parts": parts}));
}

/// JSON Schema keywords Gemini's Schema proto does not have.
///
/// Walked recursively: `additionalProperties` also sits on nested `items` and
/// `allOf` entries (handoff briefs), and `$schema` on MCP tools.
const GEMINI_UNSUPPORTED_SCHEMA_KEYS: &[&str] = &[
    "$schema",
    "$id",
    "$ref",
    "$comment",
    "$defs",
    "definitions",
    "additionalProperties",
    "additionalItems",
    "unevaluatedProperties",
    "unevaluatedItems",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "uniqueItems",
    "contentEncoding",
    "contentMediaType",
    "if",
    "then",
    "else",
    "not",
    "dependentRequired",
    "dependentSchemas",
    "prefixItems",
    "patternProperties",
    "propertyNames",
];

pub(super) fn strip_gemini_unsupported(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for key in GEMINI_UNSUPPORTED_SCHEMA_KEYS {
                map.remove(*key);
            }
            for nested in map.values_mut() {
                strip_gemini_unsupported(nested);
            }
        }
        Value::Array(items) => {
            for item in items {
                strip_gemini_unsupported(item);
            }
        }
        _ => {}
    }
}
