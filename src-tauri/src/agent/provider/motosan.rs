//! Claude Code and Codex via `motosan-ai`, Grok via the OpenAI-compatible path,
//! Gemini via the Generative Language API.
//!
//! Also an API key aimed at Anthropic's own host, which is not a CLI login at
//! all and arrives here anyway: `/v1/messages` is where prompt caching lives,
//! and the `/chat/completions` layer on the same host does not have it. The
//! credential is the only thing that differs — see [`credentials`]. Gemini is
//! the same shape: a pasted AI Studio key, a different dialect.
//!
//! Construction still never fails: a missing CLI login becomes the stream's
//! first [`ModelEvent::Error`]. Refresh happens at the start of the stream so
//! a settings panel that merely *opens* does not hit the token endpoint.

use std::collections::HashMap;

use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::agent::wire::{ModelEvent, ModelRequest, StopReason, Usage, WireMessage, WireToolCall};
use crate::error::ErrorCode;
use crate::oauth::{self, Resolved};
use crate::secrets::ApiKey;
use crate::store::{AuthKind, ProviderSettings};

use super::openai::{self, OpenAiProvider};
use super::{Provider, STREAM_BUFFER};

/// A provider that authenticates with a CLI login already on this machine, or
/// with a pasted key when the endpoint is Anthropic's own.
#[derive(Debug, Clone)]
pub struct SubscriptionProvider {
    /// The whole provider configuration, carried rather than unpacked: three
    /// of its four fields were already being threaded through this module one
    /// argument at a time, and the fourth — the output ceiling — is the one
    /// that would have made every signature here too long to read.
    settings: ProviderSettings,
    /// The pasted key, for the non-CLI kinds that reach this provider:
    /// [`AuthKind::ApiKey`] aimed at Anthropic (see
    /// [`speaks_anthropic`](super::catalog::speaks_anthropic)) and
    /// [`AuthKind::Gemini`]. `None` for every CLI login, whose credential is
    /// read from disk per stream.
    key: Option<ApiKey>,
    http: Option<reqwest::Client>,
}

impl SubscriptionProvider {
    /// Builds the provider for these settings. Infallible: a missing login, or
    /// a missing key, is reported on the first stream event.
    pub fn new(
        settings: ProviderSettings,
        key: Option<ApiKey>,
        http: Option<reqwest::Client>,
    ) -> Self {
        Self {
            settings,
            key,
            http,
        }
    }
}

impl Provider for SubscriptionProvider {
    fn model(&self) -> &str {
        &self.settings.model
    }

    fn stream(&self, request: ModelRequest) -> mpsc::Receiver<ModelEvent> {
        let (tx, rx) = mpsc::channel(STREAM_BUFFER);
        let settings = self.settings.clone();
        let key = self.key.clone();
        let http = self.http.clone();

        tokio::spawn(async move {
            run(settings, key, http, request, tx).await;
        });

        rx
    }
}

/// The credential this kind uses.
///
/// [`AuthKind::ApiKey`] only reaches this module when the endpoint is
/// Anthropic's own, so the pasted key *is* the Anthropic credential; motosan
/// reads the `sk-ant-oat01-` prefix to tell a CLI token from a key and sends
/// each in the header that one wants. [`AuthKind::Gemini`] is the same store
/// and a different header (`x-goog-api-key`). Every other kind is a login on
/// disk.
async fn credentials(
    kind: AuthKind,
    key: Option<ApiKey>,
    http: Option<&reqwest::Client>,
    base_url: &str,
) -> Result<Resolved, oauth::Missing> {
    let no_key = || {
        oauth::Missing::no_key(format!(
            "No API key. Add one in Settings, or start Aegis with {} set.",
            crate::secrets::ENV_API_KEY
        ))
    };

    match kind {
        AuthKind::ApiKey => key
            .map(|access_token| Resolved::Anthropic { access_token })
            .ok_or_else(no_key),
        AuthKind::Gemini => key
            .map(|access_token| Resolved::Gemini { access_token })
            .ok_or_else(no_key),
        AuthKind::ClaudeCli | AuthKind::CodexCli | AuthKind::GrokCli => {
            oauth::resolve(kind, http, base_url).await
        }
    }
}

async fn run(
    settings: ProviderSettings,
    key: Option<ApiKey>,
    http: Option<reqwest::Client>,
    request: ModelRequest,
    tx: mpsc::Sender<ModelEvent>,
) {
    if tx.is_closed() {
        return;
    }

    let kind = settings.auth_kind;
    let base_url = settings.base_url.clone();
    let model = settings.model.clone();

    let resolved = match credentials(kind, key, http.as_ref(), &base_url).await {
        Ok(resolved) => resolved,
        Err(missing) => {
            let _ = tx
                .send(ModelEvent::Error {
                    code: missing.code.to_owned(),
                    message: missing.message,
                    retryable: false,
                })
                .await;
            return;
        }
    };

    match resolved {
        Resolved::Anthropic { access_token } => {
            stream_motosan(
                motosan_ai::Provider::Anthropic,
                access_token.expose(),
                None,
                &settings,
                request,
                tx,
            )
            .await;
        }
        Resolved::Codex {
            access_token,
            account_id,
        } => {
            stream_motosan(
                motosan_ai::Provider::OpenAiChatGpt,
                access_token.expose(),
                Some(account_id),
                &settings,
                request,
                tx,
            )
            .await;
        }
        Resolved::Grok {
            access_token,
            base_url,
        } => {
            // Grok's proxy is OpenAI-compatible, so it goes back out through
            // that provider with the endpoint the login named.
            let grok = ProviderSettings {
                base_url,
                model,
                auth_kind: AuthKind::GrokCli,
                max_output_tokens: settings.max_output_tokens,
            };
            let inner = OpenAiProvider::with_extra_headers(
                http,
                &grok,
                Some(access_token),
                oauth::grok_headers(),
            );
            let mut stream = inner.stream(request);
            loop {
                tokio::select! {
                    biased;
                    () = tx.closed() => return,
                    event = stream.recv() => match event {
                        Some(event) => {
                            if tx.send(event).await.is_err() {
                                return;
                            }
                        }
                        None => return,
                    },
                }
            }
        }
        Resolved::Gemini { access_token } => {
            stream_gemini(access_token.expose(), &settings, request, http, tx).await;
        }
    }
}

/// Asks the endpoint a turn would use whether it will answer.
pub async fn probe(
    kind: AuthKind,
    model: &str,
    base_url: &str,
    key: Option<ApiKey>,
    http: Option<&reqwest::Client>,
) -> openai::ProviderProbe {
    let unreachable = |message: String| openai::ProviderProbe {
        ok: false,
        status: None,
        latency_ms: None,
        message,
    };

    if model.is_empty() {
        return unreachable(
            "No model is set. A request needs one, so there is nothing to test yet.".to_owned(),
        );
    }

    let started = std::time::Instant::now();
    let resolved = match credentials(kind, key, http, base_url).await {
        Ok(resolved) => resolved,
        Err(missing) => return unreachable(missing.message),
    };

    match resolved {
        Resolved::Grok {
            access_token,
            base_url,
        } => {
            let settings = crate::store::ProviderSettings {
                base_url,
                model: model.to_owned(),
                auth_kind: AuthKind::GrokCli,
                max_output_tokens: None,
            };
            return openai::probe_with_headers(
                http,
                &settings,
                Some(&access_token),
                &oauth::grok_headers(),
            )
            .await;
        }
        Resolved::Anthropic { access_token } => {
            probe_motosan(
                motosan_ai::Provider::Anthropic,
                access_token.expose(),
                None,
                model,
                base_url,
                started,
            )
            .await
        }
        Resolved::Codex {
            access_token,
            account_id,
        } => {
            probe_motosan(
                motosan_ai::Provider::OpenAiChatGpt,
                access_token.expose(),
                Some(account_id),
                model,
                base_url,
                started,
            )
            .await
        }
        Resolved::Gemini { access_token } => {
            probe_motosan(
                motosan_ai::Provider::Gemini,
                access_token.expose(),
                None,
                model,
                base_url,
                started,
            )
            .await
        }
    }
}

fn build_client(
    provider: motosan_ai::Provider,
    access_token: &str,
    account_id: Option<String>,
    model: &str,
    base_url: &str,
) -> Result<motosan_ai::Client, String> {
    let mut builder = motosan_ai::Client::builder()
        .provider(provider)
        .model(model);

    match provider {
        motosan_ai::Provider::Anthropic => {
            builder = builder.api_key(access_token);
            let origin = super::catalog::anthropic_origin(&super::catalog::effective_base_url(
                AuthKind::ClaudeCli,
                base_url,
            ));
            if !origin.is_empty() {
                builder = builder.anthropic_base_url(origin);
            }
        }
        motosan_ai::Provider::OpenAiChatGpt => {
            let account_id = account_id
                .ok_or_else(|| "The Codex login has no ChatGPT account id.".to_owned())?;
            builder = builder.chatgpt_codex(access_token, account_id, model);
        }
        _ => {
            return Err("this CLI login is not wired to a motosan-ai provider".to_owned());
        }
    }

    builder
        .build()
        .map_err(|err| format!("The CLI client could not be built ({err})."))
}

fn gemini_provider(
    access_token: &str,
    model: &str,
    base_url: &str,
) -> motosan_ai::providers::gemini::GeminiProvider {
    let origin = super::catalog::effective_base_url(AuthKind::Gemini, base_url);
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
async fn stream_gemini(
    access_token: &str,
    settings: &ProviderSettings,
    request: ModelRequest,
    http: Option<reqwest::Client>,
    tx: mpsc::Sender<ModelEvent>,
) {
    let body = gemini_body(&request, settings.max_output_tokens);
    let base = super::catalog::effective_base_url(AuthKind::Gemini, &settings.base_url);
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

    let mut decoder = super::openai::SseDecoder::default();
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
                    if !thought && !text.is_empty() {
                        if tx
                            .send(ModelEvent::TextDelta {
                                text: text.to_owned(),
                            })
                            .await
                            .is_err()
                        {
                            return;
                        }
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

fn thought_signature_of(part: &Value) -> Option<String> {
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
fn gemini_body(request: &ModelRequest, max_tokens: Option<u32>) -> Value {
    let mut contents: Vec<Value> = Vec::new();
    let mut system: Vec<String> = Vec::new();
    let mut names: HashMap<String, String> = HashMap::new();

    for message in &request.messages {
        match message {
            WireMessage::System { content } => system.push(content.clone()),
            WireMessage::User { content } => {
                contents.push(json!({"role": "user", "parts": [{"text": content}]}));
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
            } => {
                let name = names
                    .get(tool_call_id)
                    .cloned()
                    .unwrap_or_else(|| tool_call_id.clone());
                let response: Value =
                    serde_json::from_str(content).unwrap_or_else(|_| json!({"result": content}));
                contents.push(json!({
                    "role": "user",
                    "parts": [{"functionResponse": {"name": name, "response": response}}]
                }));
            }
        }
    }

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

fn codex_provider(
    access_token: &str,
    account_id: String,
    model: &str,
    base_url: &str,
) -> motosan_ai::providers::chatgpt_codex::ChatGptCodexProvider {
    let responses = super::catalog::codex_responses_url(&super::catalog::effective_base_url(
        AuthKind::CodexCli,
        base_url,
    ));
    motosan_ai::providers::chatgpt_codex::ChatGptCodexProvider::new(
        access_token,
        account_id,
        model,
        Some(responses),
    )
}

async fn stream_motosan(
    provider: motosan_ai::Provider,
    access_token: &str,
    account_id: Option<String>,
    settings: &ProviderSettings,
    request: ModelRequest,
    tx: mpsc::Sender<ModelEvent>,
) {
    let (model, base_url) = (settings.model.as_str(), settings.base_url.as_str());
    // Gemini-specific: remap tool results onto the function name, and strip
    // JSON Schema keywords its proto does not have.
    let remap_tool_names = matches!(provider, motosan_ai::Provider::Gemini);
    let chat = match to_chat_request(
        &request,
        model,
        settings.max_output_tokens,
        remap_tool_names,
    ) {
        Ok(chat) => chat,
        Err(message) => {
            fail(&tx, ErrorCode::ProviderParse, message, false).await;
            return;
        }
    };

    let mut stream =
        match open_stream(provider, access_token, account_id, model, base_url, chat).await {
            Ok(stream) => stream,
            Err(err) => {
                let retryable = is_retryable(&err);
                fail(
                    &tx,
                    ErrorCode::ProviderHttp,
                    motosan_message(&err),
                    retryable,
                )
                .await;
                return;
            }
        };

    let mut usage: Option<Usage> = None;
    let mut saw_tool_call = false;
    let mut reason = None;
    let mut tool_index: HashMap<String, u32> = HashMap::new();
    let mut next_index = 0u32;

    loop {
        let item = tokio::select! {
            biased;
            () = tx.closed() => return,
            item = stream.next() => item,
        };

        let event = match item {
            Some(Ok(event)) => event,
            Some(Err(err)) => {
                fail(
                    &tx,
                    ErrorCode::ProviderHttp,
                    motosan_message(&err),
                    is_retryable(&err),
                )
                .await;
                return;
            }
            None => break,
        };

        match event.event_type {
            motosan_ai::StreamEventType::Text => {
                if !event.content.is_empty()
                    && tx
                        .send(ModelEvent::TextDelta {
                            text: event.content,
                        })
                        .await
                        .is_err()
                {
                    return;
                }
            }
            motosan_ai::StreamEventType::ToolCallStart => {
                saw_tool_call = true;
                let id = event.tool_call_id.clone().unwrap_or_default();
                let index = *tool_index.entry(id.clone()).or_insert_with(|| {
                    let assigned = next_index;
                    next_index = next_index.saturating_add(1);
                    assigned
                });
                if tx
                    .send(ModelEvent::ToolCallDelta {
                        index,
                        id: event.tool_call_id,
                        name: event.tool_call_name,
                        args_delta: String::new(),
                        thought_signature: None,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
            }
            motosan_ai::StreamEventType::ToolCallArgs => {
                saw_tool_call = true;
                let index = event
                    .tool_call_id
                    .as_deref()
                    .and_then(|id| tool_index.get(id).copied())
                    .unwrap_or(0);
                if tx
                    .send(ModelEvent::ToolCallDelta {
                        index,
                        id: None,
                        name: None,
                        args_delta: event.tool_call_args_delta.unwrap_or_default(),
                        thought_signature: None,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
            }
            motosan_ai::StreamEventType::ToolCallEnd => {}
            motosan_ai::StreamEventType::Usage => {
                if let Some(found) = event.usage {
                    let round = to_usage(provider, &found);
                    match &mut usage {
                        Some(spent) => spent.add(round),
                        None => usage = Some(round),
                    }
                }
            }
            motosan_ai::StreamEventType::ThinkingDelta
            | motosan_ai::StreamEventType::ThinkingDone => {}
        }

        if let Some(stop) = event.stop_reason {
            reason = Some(map_stop(stop));
        }

        if event.done {
            let reason = reason.unwrap_or(if saw_tool_call {
                StopReason::ToolCalls
            } else {
                StopReason::Stop
            });
            let _ = tx.send(ModelEvent::Finish { reason, usage }).await;
            return;
        }
    }
}

async fn probe_motosan(
    provider: motosan_ai::Provider,
    access_token: &str,
    account_id: Option<String>,
    model: &str,
    base_url: &str,
    started: std::time::Instant,
) -> openai::ProviderProbe {
    let unreachable = |message: String| openai::ProviderProbe {
        ok: false,
        status: None,
        latency_ms: None,
        message,
    };

    let request = motosan_ai::ChatRequest::builder()
        .messages(vec![motosan_ai::Message::user("Reply with the word ok.")])
        .model(model)
        .max_tokens(16)
        .build();

    match open_chat(provider, access_token, account_id, model, base_url, request).await {
        Ok(response) => {
            let latency = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            let answered = if response.model.is_empty() {
                model.to_owned()
            } else {
                response.model
            };
            openai::ProviderProbe {
                ok: true,
                status: Some(200),
                latency_ms: Some(latency),
                message: if answered == model {
                    format!("The server answered as `{answered}`.")
                } else {
                    format!(
                        "The server answered, as `{answered}` rather than the `{model}` that was asked for."
                    )
                },
            }
        }
        Err(err) => unreachable(motosan_message(&err)),
    }
}

/// One round's usage, in the shape [`Usage`] promises.
///
/// The whole function is one disagreement between two APIs. Anthropic reports
/// `input_tokens` *net* of the cached tokens — a turn that read 40k from the
/// cache and sent 200 new ones says `input_tokens: 200` — so the two cache
/// figures are added back to make `prompt_tokens` mean "the prompt". The
/// Responses API counts them in already (`input_tokens_details.cached_tokens`
/// is a share of `input_tokens`, not a sibling of it), so adding there would
/// count the cache twice and report a turn as more expensive the better it
/// went.
fn to_usage(provider: motosan_ai::Provider, found: &motosan_ai::Usage) -> Usage {
    let read = u64::from(found.cache_read_input_tokens.unwrap_or(0));
    let created = u64::from(found.cache_creation_input_tokens.unwrap_or(0));
    let input = u64::from(found.input_tokens);
    let completion = u64::from(found.output_tokens);

    let prompt = if matches!(provider, motosan_ai::Provider::Anthropic) {
        input.saturating_add(read).saturating_add(created)
    } else {
        input
    };

    Usage {
        prompt_tokens: prompt,
        cache_read_tokens: read,
        cache_creation_tokens: created,
        completion_tokens: completion,
        total_tokens: prompt.saturating_add(completion),
    }
}

fn to_chat_request(
    request: &ModelRequest,
    model: &str,
    max_tokens: Option<u32>,
    remap_tool_names: bool,
) -> Result<motosan_ai::ChatRequest, String> {
    let mut system = Vec::new();
    let mut messages = Vec::new();
    // Gemini's `functionResponse.name` is the function name, not the opaque
    // call id motosan generates on the stream (`call_0`, `call_1`, …). The
    // map is filled from assistant turns in this same request, which is
    // where the name still lives.
    let mut names: HashMap<String, String> = HashMap::new();

    for message in &request.messages {
        match message {
            WireMessage::System { content } => system.push(content.clone()),
            WireMessage::Assistant { tool_calls, .. } => {
                for call in tool_calls {
                    names.insert(call.id.clone(), call.function.name.clone());
                }
                messages.push(to_motosan_message(message, None)?);
            }
            WireMessage::Tool { tool_call_id, .. } => {
                let mapped = if remap_tool_names {
                    names.get(tool_call_id).cloned()
                } else {
                    None
                };
                messages.push(to_motosan_message(message, mapped.as_deref())?);
            }
            other => messages.push(to_motosan_message(other, None)?),
        }
    }

    mark_cache_breakpoint(&mut messages);

    let mut builder = motosan_ai::ChatRequest::builder()
        .messages(messages)
        .model(model);

    if !system.is_empty() {
        // The second breakpoint. Anthropic renders `tools` then `system` then
        // `messages`, so this one alone would cover the tool schemas too — the
        // separate one below is what survives a system message that moved,
        // which in this app is every request that re-reads a shared digest.
        builder = builder.system_cached(system.join("\n\n"));
    }

    let tools = tools_of(&request.tools, remap_tool_names);
    if !tools.is_empty() {
        builder = builder.tools_cached(tools);
    }

    // The ceiling the provider's own catalog reported for this model, which is
    // the whole reason it is carried this far. Left unset when the catalog
    // would not say, because motosan's fallback is safe on every model and a
    // guessed number above the model's real limit is a 400 on every turn.
    //
    // It is not only a limit on how long a reply may be: a file written by
    // `fs_write` is emitted as the arguments of a tool call, so this is also
    // the largest file a turn can write. motosan's own default of 8192 caps
    // that at roughly 25 KB of source once JSON escaping is counted, and
    // overflowing it truncates the arguments mid-JSON — which the assembler
    // then refuses to parse, so the call is answered rather than run and the
    // file is silently never written.
    if let Some(max_tokens) = max_tokens {
        builder = builder.max_tokens(max_tokens);
    }

    Ok(builder.build())
}

/// Marks where the conversation may be read back out of the cache.
///
/// Caching is a prefix match, so one breakpoint on the newest message makes
/// every earlier byte re-readable: the request a turn sends is the whole
/// transcript again, and without this each round of a tool loop pays full
/// price for the round before it. Three breakpoints in all — tools, system,
/// here — against a limit of four.
///
/// The last message is skipped when it is a tool result, because motosan
/// serializes `Role::Tool` without ever consulting the flag (`cache` is read
/// on the user and assistant arms only). Marking one would be a breakpoint
/// that silently is not there, so the mark goes on the assistant turn that
/// asked for the call instead. The cost is one round of lag: this round's tool
/// output is written to the cache by the next round, which is the round that
/// reads it back.
fn mark_cache_breakpoint(messages: &mut [motosan_ai::Message]) {
    if let Some(message) = messages
        .iter_mut()
        .rev()
        .find(|message| !matches!(message.role, motosan_ai::Role::Tool))
    {
        message.cache = true;
    }
}

fn to_motosan_message(
    message: &WireMessage,
    tool_name: Option<&str>,
) -> Result<motosan_ai::Message, String> {
    match message {
        WireMessage::System { content } => Ok(motosan_ai::Message::system(content)),
        WireMessage::User { content } => Ok(motosan_ai::Message::user(content)),
        WireMessage::Assistant {
            content,
            tool_calls,
        } => {
            if tool_calls.is_empty() {
                Ok(motosan_ai::Message::assistant(
                    content.clone().unwrap_or_default(),
                ))
            } else {
                Ok(motosan_ai::Message::assistant_with_tool_calls(
                    content.as_deref().unwrap_or(""),
                    tool_calls.iter().map(to_motosan_tool_call).collect(),
                ))
            }
        }
        WireMessage::Tool {
            tool_call_id,
            content,
        } => Ok(motosan_ai::Message::tool_result(
            tool_name.unwrap_or(tool_call_id),
            content,
        )),
    }
}

fn to_motosan_tool_call(call: &WireToolCall) -> motosan_ai::ToolCall {
    let input = serde_json::from_str(&call.function.arguments).unwrap_or_else(|_| json!({}));
    motosan_ai::ToolCall {
        id: call.id.clone(),
        name: call.function.name.clone(),
        input,
    }
}

fn tools_of(values: &[Value], for_gemini: bool) -> Vec<motosan_ai::Tool> {
    values
        .iter()
        .filter_map(|value| {
            let function = value.get("function")?;
            let mut input_schema = function
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| json!({ "type": "object" }));
            if for_gemini {
                // Gemini's FunctionDeclaration Schema is a proto, not JSON
                // Schema: unknown fields fail the whole request rather than
                // being ignored. Aegis's own tools carry `additionalProperties`,
                // MCP tools often carry `$schema`.
                strip_gemini_unsupported(&mut input_schema);
            }
            Some(motosan_ai::Tool::from(motosan_ai::ToolSchema {
                name: function.get("name")?.as_str()?.to_owned(),
                description: function
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                input_schema,
            }))
        })
        .collect()
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

fn strip_gemini_unsupported(value: &mut Value) {
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

fn map_stop(reason: motosan_ai::StopReason) -> StopReason {
    match reason {
        motosan_ai::StopReason::ToolUse => StopReason::ToolCalls,
        motosan_ai::StopReason::MaxTokens => StopReason::Length,
        motosan_ai::StopReason::EndTurn | motosan_ai::StopReason::Stop => StopReason::Stop,
        _ => StopReason::Stop,
    }
}

fn motosan_message(err: &motosan_ai::MotosanError) -> String {
    format!("{err}")
}

async fn open_stream(
    provider: motosan_ai::Provider,
    access_token: &str,
    account_id: Option<String>,
    model: &str,
    base_url: &str,
    chat: motosan_ai::ChatRequest,
) -> Result<motosan_ai::BoxStream, motosan_ai::MotosanError> {
    use motosan_ai::providers::ProviderImpl;

    if matches!(provider, motosan_ai::Provider::OpenAiChatGpt) {
        let account_id = account_id.ok_or_else(|| {
            motosan_ai::MotosanError::Config("The Codex login has no ChatGPT account id.".into())
        })?;
        return codex_provider(access_token, account_id, model, base_url)
            .stream(chat)
            .await;
    }

    if matches!(provider, motosan_ai::Provider::Gemini) {
        return gemini_provider(access_token, model, base_url)
            .stream(chat)
            .await;
    }

    let client = build_client(provider, access_token, account_id, model, base_url)
        .map_err(motosan_ai::MotosanError::Config)?;
    client.stream_with(chat).await
}

async fn open_chat(
    provider: motosan_ai::Provider,
    access_token: &str,
    account_id: Option<String>,
    model: &str,
    base_url: &str,
    chat: motosan_ai::ChatRequest,
) -> Result<motosan_ai::ChatResponse, motosan_ai::MotosanError> {
    use motosan_ai::providers::ProviderImpl;

    if matches!(provider, motosan_ai::Provider::OpenAiChatGpt) {
        let account_id = account_id.ok_or_else(|| {
            motosan_ai::MotosanError::Config("The Codex login has no ChatGPT account id.".into())
        })?;
        return codex_provider(access_token, account_id, model, base_url)
            .chat(chat)
            .await;
    }

    if matches!(provider, motosan_ai::Provider::Gemini) {
        return gemini_provider(access_token, model, base_url)
            .chat(chat)
            .await;
    }

    let client = build_client(provider, access_token, account_id, model, base_url)
        .map_err(motosan_ai::MotosanError::Config)?;
    client.chat_with(chat).await
}

fn is_retryable(err: &motosan_ai::MotosanError) -> bool {
    matches!(
        err,
        motosan_ai::MotosanError::RateLimit { .. }
            | motosan_ai::MotosanError::Network(_)
            | motosan_ai::MotosanError::StreamReadTimeout(_)
    )
}

async fn fail(tx: &mpsc::Sender<ModelEvent>, code: ErrorCode, message: String, retryable: bool) {
    let _ = tx
        .send(ModelEvent::Error {
            code: code.as_str().to_owned(),
            message,
            retryable,
        })
        .await;
}

#[cfg(test)]
mod tests {
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
                    },
                    WireMessage::Assistant {
                        content: None,
                        tool_calls: vec![WireToolCall::new("call-1", "fs_read", "{}")],
                    },
                    WireMessage::Tool {
                        tool_call_id: "call-1".to_owned(),
                        content: "{\"ok\":true}".to_owned(),
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
                    },
                    WireMessage::Assistant {
                        content: None,
                        tool_calls: vec![WireToolCall::new("call_0", "fs_write", "{}")],
                    },
                    WireMessage::Tool {
                        tool_call_id: "call_0".to_owned(),
                        content: "{\"ok\":true}".to_owned(),
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

        let anthropic =
            to_chat_request(&request(Vec::new(), tools), "claude-sonnet-5", None, false)
                .expect("the request converts");
        let kept = &anthropic.tools.as_ref().expect("tools")[0].input_schema;
        assert_eq!(kept["additionalProperties"], false);
        assert_eq!(
            kept["$schema"],
            "https://json-schema.org/draft/2020-12/schema"
        );
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
                    },
                    WireMessage::Assistant {
                        content: None,
                        tool_calls: vec![call],
                    },
                    WireMessage::Tool {
                        tool_call_id: "call_0".to_owned(),
                        content: "{\"ok\":true}".to_owned(),
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
}
