//! Claude Code and Codex via `motosan-ai`, Grok via the OpenAI-compatible path,
//! Gemini via the Generative Language API.
//!
//! Anthropic API keys come here too, for `/v1/messages` prompt caching; only
//! the credential differs ([`credentials`]). Gemini likewise takes a pasted key.
//!
//! Construction never fails (errors are the stream's first
//! [`ModelEvent::Error`]), and tokens refresh when the stream starts, not when
//! Settings opens.

use std::collections::HashMap;

use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::agent::wire::{
    with_notes, ModelEvent, ModelRequest, StopReason, Usage, WireImage, WireMessage, WireToolCall,
    TOOL_IMAGES_LEAD,
};
use crate::error::ErrorCode;
use crate::oauth::{self, Resolved};
use crate::secrets::ApiKey;
use crate::store::{AuthKind, ProviderSettings};

use super::openai::{self, OpenAiProvider};
use super::{Provider, STREAM_BUFFER};

mod gemini;

use gemini::{gemini_provider, stream_gemini, strip_gemini_unsupported};

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
    /// Aegis session id, sent as the Grok prompt-cache key. `None` for a
    /// provider built with no session (the settings probe).
    conversation_id: Option<String>,
}

impl SubscriptionProvider {
    /// Builds the provider for these settings. Infallible: a missing login, or
    /// a missing key, is reported on the first stream event.
    pub fn new(
        settings: ProviderSettings,
        key: Option<ApiKey>,
        http: Option<reqwest::Client>,
        conversation_id: Option<String>,
    ) -> Self {
        Self {
            settings,
            key,
            http,
            conversation_id,
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
        let conversation_id = self.conversation_id.clone();

        tokio::spawn(async move {
            run(settings, key, http, conversation_id, request, tx).await;
        });

        rx
    }
}

/// The credential this kind uses: the pasted key for [`AuthKind::ApiKey`]
/// (Anthropic's host only; motosan picks the header from the `sk-ant-oat01-`
/// prefix) and [`AuthKind::Gemini`] (`x-goog-api-key`), a login on disk
/// otherwise.
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
    conversation_id: Option<String>,
    mut request: ModelRequest,
    tx: mpsc::Sender<ModelEvent>,
) {
    if tx.is_closed() {
        return;
    }
    super::image::load(&mut request).await;

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
            // motosan's Codex dialect is text-only in this version: said to the
            // model rather than silently dropped (PLAN 7.20).
            super::image::refuse_all(
                &mut request,
                "this provider's dialect does not carry images in this build",
            );
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
            // The CLI catalog's models are sampled on `/responses`. The proxy
            // routes on `x-grok-model-override` and caches on `x-grok-conv-id`.
            let grok = ProviderSettings {
                base_url,
                model: model.clone(),
                auth_kind: AuthKind::GrokCli,
                max_output_tokens: settings.max_output_tokens,
                prices: Vec::new(),
            };
            let inner = OpenAiProvider::responses(
                http,
                &grok,
                Some(access_token),
                oauth::grok_request_headers(&model, conversation_id.as_deref()),
                conversation_id,
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
                prices: Vec::new(),
            };
            return openai::probe_responses(
                http,
                &settings,
                Some(&access_token),
                &oauth::grok_request_headers(model, None),
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
/// Anthropic's `input_tokens` excludes cached tokens, so both cache figures are
/// added back; the Responses API already includes them
/// (`input_tokens_details.cached_tokens`), so nothing is added there.
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
    // A tool result carries text only here, so a round's capture follows the
    // results as one user turn (PLAN 7.20).
    let mut pending: Vec<motosan_ai::ContentBlock> = Vec::new();

    for message in &request.messages {
        if !matches!(message, WireMessage::Tool { .. }) {
            flush_motosan_images(&mut messages, &mut pending);
        }
        match message {
            WireMessage::System { content } => system.push(content.clone()),
            WireMessage::Assistant { tool_calls, .. } => {
                for call in tool_calls {
                    names.insert(call.id.clone(), call.function.name.clone());
                }
                messages.push(to_motosan_message(message, None)?);
            }
            WireMessage::Tool {
                tool_call_id,
                images,
                ..
            } => {
                let mapped = if remap_tool_names {
                    names.get(tool_call_id).cloned()
                } else {
                    None
                };
                messages.push(to_motosan_message(message, mapped.as_deref())?);
                pending.extend(images.iter().filter_map(motosan_image));
            }
            other => messages.push(to_motosan_message(other, None)?),
        }
    }
    flush_motosan_images(&mut messages, &mut pending);

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

    // The catalog's output ceiling, unset when unknown (a guess too high is a
    // 400 every turn). It also bounds `fs_write`, whose content is emitted as
    // arguments: motosan's 8192 default caps a file near 25 KB.
    if let Some(max_tokens) = max_tokens {
        builder = builder.max_tokens(max_tokens);
    }

    Ok(builder.build())
}

/// Marks where the conversation may be read back out of the cache.
///
/// One breakpoint on the newest message caches the whole prefix (three of the
/// four allowed: tools, system, here). motosan ignores the flag on
/// `Role::Tool`, so a trailing tool result puts the mark on the assistant turn
/// before it, one round of lag.
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
        WireMessage::User { content, images } => {
            let text = with_notes(content, images);
            let blocks: Vec<motosan_ai::ContentBlock> =
                images.iter().filter_map(motosan_image).collect();
            if blocks.is_empty() {
                return Ok(motosan_ai::Message::user(text));
            }
            // An empty text block is refused by Anthropic; an image alone is not.
            let mut all = Vec::with_capacity(blocks.len() + 1);
            if !text.is_empty() {
                all.push(motosan_ai::ContentBlock::Text { text });
            }
            all.extend(blocks);
            Ok(motosan_ai::Message::user_with_blocks(all))
        }
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
            images,
        } => Ok(motosan_ai::Message::tool_result(
            tool_name.unwrap_or(tool_call_id),
            with_notes(content, images),
        )),
    }
}

fn motosan_image(image: &WireImage) -> Option<motosan_ai::ContentBlock> {
    match image {
        WireImage::Inline { mime, data } => Some(motosan_ai::ContentBlock::Image {
            source: motosan_ai::ImageSource::Base64 {
                media_type: mime.clone(),
                data: data.clone(),
            },
        }),
        WireImage::File { .. } | WireImage::Missing { .. } => None,
    }
}

fn flush_motosan_images(
    messages: &mut Vec<motosan_ai::Message>,
    pending: &mut Vec<motosan_ai::ContentBlock>,
) {
    if pending.is_empty() {
        return;
    }
    let mut blocks = vec![motosan_ai::ContentBlock::Text {
        text: TOOL_IMAGES_LEAD.to_owned(),
    }];
    blocks.append(pending);
    messages.push(motosan_ai::Message::user_with_blocks(blocks));
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
mod tests;
