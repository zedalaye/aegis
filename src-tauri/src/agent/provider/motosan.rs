//! Claude Code and Codex via `motosan-ai`, Grok via the OpenAI-compatible path.
//!
//! Also an API key aimed at Anthropic's own host, which is not a CLI login at
//! all and arrives here anyway: `/v1/messages` is where prompt caching lives,
//! and the `/chat/completions` layer on the same host does not have it. The
//! credential is the only thing that differs — see [`credentials`].
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
use crate::store::AuthKind;

use super::openai::{self, OpenAiProvider};
use super::{Provider, STREAM_BUFFER};

/// A provider that authenticates with a CLI login already on this machine, or
/// with a pasted key when the endpoint is Anthropic's own.
#[derive(Debug, Clone)]
pub struct SubscriptionProvider {
    kind: AuthKind,
    model: String,
    /// Optional base-URL override (Grok). Empty means the CLI's own endpoint.
    base_url: String,
    /// The pasted key, for the one non-CLI kind that reaches this provider:
    /// [`AuthKind::ApiKey`] aimed at Anthropic (see
    /// [`speaks_anthropic`](super::catalog::speaks_anthropic)). `None` for
    /// every CLI login, whose credential is read from disk per stream.
    key: Option<ApiKey>,
    http: Option<reqwest::Client>,
}

impl SubscriptionProvider {
    /// Builds the provider for this kind. Infallible: a missing login, or a
    /// missing key, is reported on the first stream event.
    pub fn new(
        kind: AuthKind,
        model: String,
        base_url: String,
        key: Option<ApiKey>,
        http: Option<reqwest::Client>,
    ) -> Self {
        Self {
            kind,
            model,
            base_url,
            key,
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
        let key = self.key.clone();
        let http = self.http.clone();

        tokio::spawn(async move {
            run(kind, model, base_url, key, http, request, tx).await;
        });

        rx
    }
}

/// The credential this kind uses.
///
/// [`AuthKind::ApiKey`] only reaches this module when the endpoint is
/// Anthropic's own, so the pasted key *is* the Anthropic credential; motosan
/// reads the `sk-ant-oat01-` prefix to tell a CLI token from a key and sends
/// each in the header that one wants. Every other kind is a login on disk.
async fn credentials(
    kind: AuthKind,
    key: Option<ApiKey>,
    http: Option<&reqwest::Client>,
    base_url: &str,
) -> Result<Resolved, oauth::Missing> {
    if matches!(kind, AuthKind::ApiKey) {
        return key
            .map(|access_token| Resolved::Anthropic { access_token })
            .ok_or_else(|| {
                oauth::Missing::no_key(format!(
                    "No API key. Add one in Settings, or start Aegis with {} set.",
                    crate::secrets::ENV_API_KEY
                ))
            });
    }

    oauth::resolve(kind, http, base_url).await
}

#[allow(clippy::too_many_arguments)]
async fn run(
    kind: AuthKind,
    model: String,
    base_url: String,
    key: Option<ApiKey>,
    http: Option<reqwest::Client>,
    request: ModelRequest,
    tx: mpsc::Sender<ModelEvent>,
) {
    if tx.is_closed() {
        return;
    }

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

fn to_chat_request(request: &ModelRequest, model: &str) -> Result<motosan_ai::ChatRequest, String> {
    let mut system = Vec::new();
    let mut messages = Vec::new();

    for message in &request.messages {
        match message {
            WireMessage::System { content } => system.push(content.clone()),
            other => messages.push(to_motosan_message(other)?),
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

    let tools = tools_of(&request.tools);
    if !tools.is_empty() {
        builder = builder.tools_cached(tools);
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
        let chat = to_chat_request(&request(Vec::new(), Vec::new()), "claude-sonnet-5")
            .expect("the request converts");

        assert!(!chat.system_cache);
        assert!(chat.tools.is_none());
        assert!(chat.messages.is_empty());
    }
}
