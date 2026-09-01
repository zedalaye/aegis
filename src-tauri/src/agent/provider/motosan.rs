//! Claude Code and Codex via `motosan-ai`, Grok via the OpenAI-compatible path.
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
use crate::store::AuthKind;

use super::openai::{self, OpenAiProvider};
use super::{Provider, STREAM_BUFFER};

/// A provider that authenticates with a CLI login already on this machine.
#[derive(Debug, Clone)]
pub struct SubscriptionProvider {
    kind: AuthKind,
    model: String,
    /// Optional base-URL override (Grok). Empty means the CLI's own endpoint.
    base_url: String,
    http: Option<reqwest::Client>,
}

impl SubscriptionProvider {
    /// Builds the provider for this CLI kind. Infallible: a missing login is
    /// reported on the first stream event.
    pub fn new(
        kind: AuthKind,
        model: String,
        base_url: String,
        http: Option<reqwest::Client>,
    ) -> Self {
        Self {
            kind,
            model,
            base_url,
            http,
        }
    }
}

impl Provider for SubscriptionProvider {
    fn model(&self) -> &str {
        &self.model
    }

    fn stream(&self, request: ModelRequest) -> mpsc::Receiver<ModelEvent> {
        let (tx, rx) = mpsc::channel(STREAM_BUFFER);
        let kind = self.kind;
        let model = self.model.clone();
        let base_url = self.base_url.clone();
        let http = self.http.clone();

        tokio::spawn(async move {
            run(kind, model, base_url, http, request, tx).await;
        });

        rx
    }
}

async fn run(
    kind: AuthKind,
    model: String,
    base_url: String,
    http: Option<reqwest::Client>,
    request: ModelRequest,
    tx: mpsc::Sender<ModelEvent>,
) {
    if tx.is_closed() {
        return;
    }

    let resolved = match oauth::resolve(kind, http.as_ref(), &base_url).await {
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
                &model,
                &base_url,
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
                &model,
                &base_url,
                request,
                tx,
            )
            .await;
        }
        Resolved::Grok {
            access_token,
            base_url,
        } => {
            let settings = crate::store::ProviderSettings {
                base_url,
                model,
                auth_kind: AuthKind::GrokCli,
            };
            let inner = OpenAiProvider::with_extra_headers(
                http,
                &settings,
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
    }
}

/// Asks the CLI's own endpoint whether it will answer.
pub async fn probe(
    kind: AuthKind,
    model: &str,
    base_url: &str,
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
    let resolved = match oauth::resolve(kind, http, base_url).await {
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
    model: &str,
    base_url: &str,
    request: ModelRequest,
    tx: mpsc::Sender<ModelEvent>,
) {
    let chat = match to_chat_request(&request, model) {
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

    let mut usage = None;
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
                    usage = Some(Usage {
                        prompt_tokens: found.input_tokens as u64,
                        completion_tokens: found.output_tokens as u64,
                        total_tokens: (found.input_tokens as u64)
                            .saturating_add(found.output_tokens as u64),
                    });
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

fn to_chat_request(request: &ModelRequest, model: &str) -> Result<motosan_ai::ChatRequest, String> {
    let mut system = Vec::new();
    let mut messages = Vec::new();

    for message in &request.messages {
        match message {
            WireMessage::System { content } => system.push(content.clone()),
            other => messages.push(to_motosan_message(other)?),
        }
    }

    let mut builder = motosan_ai::ChatRequest::builder()
        .messages(messages)
        .model(model);

    if !system.is_empty() {
        builder = builder.system(system.join("\n\n"));
    }

    let tools = tools_of(&request.tools);
    if !tools.is_empty() {
        builder = builder.tools(tools);
    }

    Ok(builder.build())
}

fn to_motosan_message(message: &WireMessage) -> Result<motosan_ai::Message, String> {
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
        } => Ok(motosan_ai::Message::tool_result(tool_call_id, content)),
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

fn tools_of(values: &[Value]) -> Vec<motosan_ai::Tool> {
    values
        .iter()
        .filter_map(|value| {
            let function = value.get("function")?;
            Some(motosan_ai::Tool::from(motosan_ai::ToolSchema {
                name: function.get("name")?.as_str()?.to_owned(),
                description: function
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                input_schema: function
                    .get("parameters")
                    .cloned()
                    .unwrap_or_else(|| json!({ "type": "object" })),
            }))
        })
        .collect()
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
