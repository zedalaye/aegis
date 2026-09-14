//! The OpenAI-compatible provider (PLAN 4.1).
//!
//! One streamed `POST {base_url}/chat/completions` per round, decoded into
//! [`ModelEvent`]s; the wire format stays in this file.
//!
//! * **Construction never fails**: a missing key, bad URL or missing client
//!   becomes the stream's single [`ModelEvent::Error`], so the user's message is
//!   still recorded.
//! * **The finish is deferred** to `[DONE]` or end of body, since usage often
//!   arrives in a later chunk and the turn loop stops at [`ModelEvent::Finish`].
//! * **Cancellation** (a dropped receiver) closes the connection at the next
//!   chunk.
//! * **The key** is a sensitive header only: never logged, never sent to the
//!   WebView ([`secrets`](crate::secrets)).

use std::time::{Duration, Instant};

use reqwest::header::{HeaderValue, ACCEPT, AUTHORIZATION};
use reqwest::{Client, StatusCode, Url};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use ts_rs::TS;

use crate::agent::wire::{ModelEvent, ModelRequest, StopReason, Usage};
use crate::error::ErrorCode;
use crate::secrets::ApiKey;
use crate::store::settings::ProviderSettings;

use super::{Provider, STREAM_BUFFER};

/// Path appended to the base URL for a completion.
const CHAT_PATH: &str = "/chat/completions";

/// Output tokens [`probe`] asks for: not degenerate, nearly free.
const PROBE_MAX_TOKENS: u32 = 16;

/// The `data:` payload that ends an SSE stream.
const DONE: &str = "[DONE]";

/// What Aegis calls itself to a server. No machine or user identity.
const USER_AGENT: &str = concat!("Aegis/", env!("CARGO_PKG_VERSION"));

/// How long to wait for a connection before giving up.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a stream may go without producing bytes; each chunk resets it.
const READ_TIMEOUT: Duration = Duration::from_secs(120);

/// How long [`probe`] waits before reporting the server as unreachable.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Most of a server's error body that is quoted back to the user.
const ERROR_BODY_CHARS: usize = 400;

/// Builds the process's shared HTTP client (one connection pool), or `None`
/// without a TLS stack. No whole-request timeout, which would cut long
/// streams; [`READ_TIMEOUT`] covers stalls.
pub fn client() -> Option<Client> {
    Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .build()
        .inspect_err(|err| {
            tracing::error!(%err, "could not build an HTTP client");
        })
        .ok()
}

// ---------------------------------------------------------------------------
// Probe (PLAN 2.1, `settings_probe_provider`)
// ---------------------------------------------------------------------------

/// What one attempt to reach the configured server found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct ProviderProbe {
    /// Whether the server answered as a working OpenAI-compatible endpoint.
    pub ok: bool,
    /// The HTTP status, when there was one. `None` means nothing answered.
    pub status: Option<u16>,
    /// Round trip in milliseconds, when a response arrived.
    #[ts(type = "number | null")]
    pub latency_ms: Option<u64>,
    /// The diagnosis, written for whoever has to fix it.
    pub message: String,
}

/// Asks the configured server whether it is there, and reports what happened.
///
/// A real, tiny completion on the turn's endpoint rather than `GET /models`,
/// which fails on some working servers (Anthropic's needs a version header)
/// and cannot check the model id.
pub async fn probe(
    client: Option<&Client>,
    settings: &ProviderSettings,
    key: Option<&ApiKey>,
) -> ProviderProbe {
    probe_with_headers(client, settings, key, &[]).await
}

/// As [`probe`], with extra headers (the Grok CLI proxy wants an identity
/// header on top of the bearer token).
pub async fn probe_with_headers(
    client: Option<&Client>,
    settings: &ProviderSettings,
    key: Option<&ApiKey>,
    extra_headers: &[(String, String)],
) -> ProviderProbe {
    let unreachable = |message: String| ProviderProbe {
        ok: false,
        status: None,
        latency_ms: None,
        message,
    };

    if settings.base_url.is_empty() {
        return unreachable(
            "No base URL is set, so replies come from the built-in scripted provider. \
             Enter an OpenAI-compatible base URL and a model to use a real one."
                .to_owned(),
        );
    }
    if settings.model.is_empty() {
        return unreachable(
            "No model is set. A request needs one, so there is nothing to test yet.".to_owned(),
        );
    }
    let Some(client) = client else {
        return unreachable(
            "Aegis has no HTTP client on this machine, so no request could be sent.".to_owned(),
        );
    };
    let url = match endpoint(&settings.base_url, CHAT_PATH) {
        Ok(url) => url,
        Err(reason) => return unreachable(reason),
    };

    let mut request = client
        .post(url)
        .timeout(PROBE_TIMEOUT)
        // Not streamed: there is nothing to watch arrive, and a whole-response
        // deadline is exactly what a probe wants.
        .json(&json!({
            "model": settings.model,
            "max_tokens": PROBE_MAX_TOKENS,
            "messages": [{ "role": "user", "content": "Reply with the word ok." }],
        }));

    if let Some(key) = key {
        match authorization(key) {
            Ok(header) => request = request.header(AUTHORIZATION, header),
            Err(reason) => return unreachable(reason),
        }
    }
    for (name, value) in extra_headers {
        match HeaderValue::from_str(value) {
            Ok(header) => request = request.header(name.as_str(), header),
            Err(_) => return unreachable(
                "A request header could not be sent. The CLI login on this machine looks damaged."
                    .to_owned(),
            ),
        }
    }

    let started = Instant::now();
    let response = match request.send().await {
        Ok(response) => response,
        Err(err) => {
            tracing::warn!(%err, "the provider probe could not reach the server");
            return unreachable(format!("{}.", transport_reason(&err)));
        }
    };

    let status = response.status();
    let latency = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let probe = |ok: bool, message: String| ProviderProbe {
        ok,
        status: Some(status.as_u16()),
        latency_ms: Some(latency),
        message,
    };

    if status.is_success() {
        // The server names the model it actually used, which is not always the
        // one that was asked for — gateways and aliases resolve to something
        // else, and that is worth seeing before it turns up on an invoice.
        let answered = response
            .json::<Value>()
            .await
            .ok()
            .and_then(|body| Some(body.get("model")?.as_str()?.to_owned()));

        return probe(
            true,
            match answered {
                Some(model) if model != settings.model => format!(
                    "The server answered, as `{model}` rather than the `{}` that was asked for.",
                    settings.model
                ),
                Some(model) => format!("The server answered as `{model}`."),
                None => "The server answered.".to_owned(),
            },
        );
    }

    let detail = error_detail(response).await;
    let message = match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => format!(
            "The server rejected the API key ({status}). {}",
            match key {
                Some(_) => "Check that the key belongs to this base URL.",
                None => "No key was found in the credential store or the environment.",
            }
        ),
        StatusCode::NOT_FOUND => format!(
            "There is no chat endpoint at `{}{CHAT_PATH}` ({status}). The base URL is probably \
             wrong — it usually ends in `/v1`.",
            settings.base_url
        ),
        StatusCode::BAD_REQUEST => format!(
            "The server understood the request and refused it ({status}). Most often that means \
             `{}` is not a model it serves.",
            settings.model
        ),
        StatusCode::TOO_MANY_REQUESTS => {
            format!("The key works, but the server is rate limiting it ({status}).")
        }
        _ => format!("The server answered {status}."),
    };

    probe(false, join_detail(message, detail))
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// Everything a request needs, once it is known there is one to send.
#[derive(Debug, Clone)]
struct Ready {
    client: Client,
    endpoint: Url,
    key: ApiKey,
    extra_headers: Vec<(String, String)>,
}

/// Why no request can be sent, in the shape the turn loop reports.
#[derive(Debug, Clone)]
struct Unusable {
    code: &'static str,
    message: String,
    retryable: bool,
}

/// A provider that streams from an OpenAI-compatible server.
#[derive(Debug, Clone)]
pub struct OpenAiProvider {
    model: String,
    /// The request to send, or the reason there is none.
    ready: Result<Ready, Unusable>,
}

impl OpenAiProvider {
    /// Builds the provider. Infallible: problems surface as the stream's only
    /// event (see the module note).
    pub fn new(client: Option<Client>, settings: &ProviderSettings, key: Option<ApiKey>) -> Self {
        Self::with_extra_headers(client, settings, key, Vec::new())
    }

    /// As [`Self::new`], attaching extra headers to every request.
    ///
    /// Grok's CLI proxy is OpenAI-compatible except for an identity header.
    /// Putting that here keeps the SSE loop in one place.
    pub fn with_extra_headers(
        client: Option<Client>,
        settings: &ProviderSettings,
        key: Option<ApiKey>,
        extra_headers: Vec<(String, String)>,
    ) -> Self {
        let ready = Self::prepare(client, settings, key, extra_headers);

        if let Err(unusable) = &ready {
            tracing::warn!(code = unusable.code, "the provider cannot send a request");
        }

        Self {
            model: settings.model.clone(),
            ready,
        }
    }

    /// The three checks, in the order a user would fix them.
    fn prepare(
        client: Option<Client>,
        settings: &ProviderSettings,
        key: Option<ApiKey>,
        extra_headers: Vec<(String, String)>,
    ) -> Result<Ready, Unusable> {
        let Some(client) = client else {
            return Err(Unusable {
                code: ErrorCode::ProviderHttp.as_str(),
                message: "Aegis could not create an HTTP client on this machine, so no request \
                          could be sent."
                    .to_owned(),
                retryable: false,
            });
        };

        let endpoint = endpoint(&settings.base_url, CHAT_PATH).map_err(|reason| Unusable {
            code: ErrorCode::ProviderHttp.as_str(),
            message: reason,
            retryable: false,
        })?;

        // Last, because it is the one a user is most likely to be missing, and
        // the message that names the two places to put it is the one they
        // should be left holding.
        let key = key.ok_or_else(|| Unusable {
            code: ErrorCode::NoApiKey.as_str(),
            message: format!(
                "No API key. Add one in Settings, or start Aegis with {} set.",
                crate::secrets::ENV_API_KEY
            ),
            retryable: false,
        })?;

        Ok(Ready {
            client,
            endpoint,
            key,
            extra_headers,
        })
    }
}

impl Provider for OpenAiProvider {
    fn model(&self) -> &str {
        &self.model
    }

    fn stream(&self, request: ModelRequest) -> mpsc::Receiver<ModelEvent> {
        let (tx, rx) = mpsc::channel(STREAM_BUFFER);

        let ready = match self.ready.clone() {
            Ok(ready) => ready,
            Err(unusable) => {
                // A closed receiver here means the turn was cancelled between
                // being started and this line, which is ordinary.
                let _ = tx.try_send(ModelEvent::Error {
                    code: unusable.code.to_owned(),
                    message: unusable.message,
                    retryable: unusable.retryable,
                });
                return rx;
            }
        };

        tokio::spawn(async move {
            run(ready, request, tx).await;
        });

        rx
    }
}

/// Sends one request and feeds its stream into `tx`.
///
/// Ends with [`ModelEvent::Error`], [`ModelEvent::Finish`], or silently on a
/// closed channel (cancelled).
async fn run(ready: Ready, request: ModelRequest, tx: mpsc::Sender<ModelEvent>) {
    let authorization = match authorization(&ready.key) {
        Ok(header) => header,
        Err(reason) => {
            fail(&tx, ErrorCode::ProviderHttp, reason, false).await;
            return;
        }
    };

    let mut sending = ready
        .client
        .post(ready.endpoint.clone())
        .header(AUTHORIZATION, authorization)
        // Some gateways serve a buffered JSON body unless the stream is asked
        // for by content type as well as by the `stream` flag in the body.
        .header(ACCEPT, HeaderValue::from_static("text/event-stream"));

    for (name, value) in &ready.extra_headers {
        match HeaderValue::from_str(value) {
            Ok(header) => sending = sending.header(name.as_str(), header),
            Err(_) => {
                fail(
                    &tx,
                    ErrorCode::ProviderHttp,
                    "A request header could not be sent. The CLI login on this machine looks damaged."
                        .to_owned(),
                    false,
                )
                .await;
                return;
            }
        }
    }

    let sending = sending.json(&request.to_body()).send();

    let mut response = tokio::select! {
        biased;

        () = tx.closed() => return,
        sent = sending => match sent {
            Ok(response) => response,
            Err(err) => {
                tracing::warn!(%err, url = %ready.endpoint, "the provider request failed");
                let retryable = err.is_timeout() || err.is_connect();
                fail(&tx, ErrorCode::ProviderHttp, format!("{}.", transport_reason(&err)), retryable).await;
                return;
            }
        },
    };

    let status = response.status();
    if !status.is_success() {
        let detail = error_detail(response).await;
        // 5xx and 429 are worth trying again; a 4xx is a request that will be
        // wrong in exactly the same way next time.
        let retryable = status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS;

        tracing::warn!(%status, "the provider answered with a failure status");
        fail(
            &tx,
            ErrorCode::ProviderHttp,
            join_detail(http_reason(status), detail),
            retryable,
        )
        .await;
        return;
    }

    let mut decoder = SseDecoder::default();
    let mut stream = StreamState::default();

    loop {
        let chunk = tokio::select! {
            biased;

            // The turn dropped the receiver: cancelled, or the window went
            // away. Returning here closes the connection.
            () = tx.closed() => return,
            chunk = response.chunk() => chunk,
        };

        let bytes = match chunk {
            Ok(Some(bytes)) => bytes,
            Ok(None) => break,
            Err(err) => {
                tracing::warn!(%err, "the provider's stream broke mid-reply");
                fail(
                    &tx,
                    ErrorCode::ProviderHttp,
                    format!("The reply was cut short: {}.", transport_reason(&err)),
                    true,
                )
                .await;
                return;
            }
        };

        let payloads = match decoder.push(&bytes) {
            Ok(payloads) => payloads,
            Err(reason) => {
                fail(&tx, ErrorCode::ProviderParse, reason, false).await;
                return;
            }
        };

        for payload in payloads {
            if payload.trim() == DONE {
                stream.done = true;
                break;
            }
            for event in stream.absorb(&payload) {
                if tx.send(event).await.is_err() {
                    return;
                }
            }
        }

        if stream.done {
            break;
        }
    }

    // A stream that ended without ever saying why is a truncated reply, not a
    // finished one. Saying nothing lets the turn loop report exactly that
    // (`agent/turn.rs`), rather than inventing a stop reason nobody sent.
    if let Some(finish) = stream.finish() {
        let _ = tx.send(finish).await;
    }
}

/// Sends one terminal error, ignoring a receiver that has gone away.
async fn fail(tx: &mpsc::Sender<ModelEvent>, code: ErrorCode, message: String, retryable: bool) {
    let _ = tx
        .send(ModelEvent::Error {
            code: code.as_str().to_owned(),
            message,
            retryable,
        })
        .await;
}

// ---------------------------------------------------------------------------
// Response mapping
// ---------------------------------------------------------------------------

/// What has been seen so far in one response, holding the deferred finish.
#[derive(Debug, Default)]
struct StreamState {
    reason: Option<StopReason>,
    usage: Option<Usage>,
    saw_tool_call: bool,
    done: bool,
}

impl StreamState {
    /// Turns one `data:` payload into the events it carries.
    ///
    /// Non-JSON payloads (keep-alive junk) are logged and dropped.
    fn absorb(&mut self, payload: &str) -> Vec<ModelEvent> {
        let frame: Value = match serde_json::from_str(payload) {
            Ok(frame) => frame,
            Err(err) => {
                tracing::debug!(%err, "a stream frame was not JSON; skipped");
                return Vec::new();
            }
        };

        // An error can arrive with a 200 and a `data:` frame — that is how
        // several gateways report a mid-stream failure, having already
        // committed to a status.
        if let Some(error) = frame.get("error").filter(|error| !error.is_null()) {
            self.done = true;
            return vec![ModelEvent::Error {
                code: ErrorCode::ProviderHttp.as_str().to_owned(),
                message: format!("The server reported: {}", error_message(error)),
                retryable: false,
            }];
        }

        if let Some(usage) = usage_of(&frame) {
            self.usage = Some(usage);
        }

        let mut events = Vec::new();
        for choice in frame
            .get("choices")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let delta = choice.get("delta").unwrap_or(&Value::Null);

            if let Some(text) = delta.get("content").and_then(Value::as_str) {
                if !text.is_empty() {
                    events.push(ModelEvent::TextDelta {
                        text: text.to_owned(),
                    });
                }
            }

            for call in delta
                .get("tool_calls")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default()
            {
                self.saw_tool_call = true;
                events.push(tool_call_delta(call));
            }

            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                self.reason = Some(stop_reason(reason));
            }
        }

        events
    }

    /// The one finish event for this response, if it is owed.
    fn finish(self) -> Option<ModelEvent> {
        let reason = match (self.reason, self.done) {
            (Some(reason), _) => reason,
            // `[DONE]` with no `finish_reason` anywhere: the response is
            // complete, and what it did is visible from what it sent.
            (None, true) if self.saw_tool_call => StopReason::ToolCalls,
            (None, true) => StopReason::Stop,
            (None, false) => return None,
        };

        Some(ModelEvent::Finish {
            reason,
            usage: self.usage,
        })
    }
}

/// One streamed tool-call fragment.
///
/// Reassembled by [`ToolCallAssembler`](crate::agent::wire::ToolCallAssembler).
/// Empty strings count as absent, so `"id": ""` padding cannot erase the id.
fn tool_call_delta(call: &Value) -> ModelEvent {
    let text = |value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .filter(|found| !found.is_empty())
            .map(str::to_owned)
    };
    let function = call.get("function");

    ModelEvent::ToolCallDelta {
        index: call
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|index| u32::try_from(index).ok())
            .unwrap_or(0),
        id: text(call.get("id")),
        name: text(function.and_then(|function| function.get("name"))),
        args_delta: function
            .and_then(|function| function.get("arguments"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        thought_signature: None,
    }
}

/// The API's stop reasons, mapped onto ours.
///
/// An unknown reason becomes [`StopReason::Stop`].
fn stop_reason(reason: &str) -> StopReason {
    match reason {
        "tool_calls" | "function_call" => StopReason::ToolCalls,
        "length" => StopReason::Length,
        "stop" => StopReason::Stop,
        other => {
            tracing::debug!(reason = other, "an unfamiliar finish reason");
            StopReason::Stop
        }
    }
}

/// Token usage, from the chunk the server sends at the end of a stream.
///
/// Requested via `stream_options.include_usage`
/// ([`ModelRequest::to_body`](crate::agent::ModelRequest::to_body)). No usage
/// means unmeasured ([`TurnCost::unreported`](crate::store::TurnCost::unreported));
/// missing fields count as zero.
fn usage_of(frame: &Value) -> Option<Usage> {
    let usage = frame.get("usage").filter(|usage| !usage.is_null())?;
    let count = |field: &str| usage.get(field).and_then(Value::as_u64).unwrap_or(0);

    let prompt = count("prompt_tokens");
    let completion = count("completion_tokens");

    // A share of `prompt_tokens`, not added to it. Zero when unreported
    // (including Anthropic's compatibility layer).
    let cached = usage
        .get("prompt_tokens_details")
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);

    Some(Usage {
        prompt_tokens: prompt,
        cache_read_tokens: cached.min(prompt),
        cache_creation_tokens: 0,
        completion_tokens: completion,
        total_tokens: match usage.get("total_tokens").and_then(Value::as_u64) {
            Some(total) => total,
            None => prompt.saturating_add(completion),
        },
    })
}

// ---------------------------------------------------------------------------
// SSE decoding
// ---------------------------------------------------------------------------

/// Bytes that may be held while waiting for a line to end.
const MAX_PENDING_BYTES: usize = 8 * 1024 * 1024;

/// Reassembles `data:` payloads from a byte stream.
///
/// Works on bytes and decodes only complete lines, so chunks may split lines,
/// CRLFs or UTF-8 sequences. Only `data` fields are kept.
#[derive(Debug, Default)]
pub struct SseDecoder {
    pending: Vec<u8>,
    data: String,
    saw_data: bool,
}

impl SseDecoder {
    /// Folds in one chunk and returns every payload it completed. Errors only
    /// on the buffer cap.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>, String> {
        if self.pending.len().saturating_add(chunk.len()) > MAX_PENDING_BYTES {
            return Err("The server sent a stream frame too large to read.".to_owned());
        }
        self.pending.extend_from_slice(chunk);

        let mut payloads = Vec::new();
        while let Some(line) = self.take_line() {
            if let Some(payload) = self.absorb_line(&line) {
                payloads.push(payload);
            }
        }
        Ok(payloads)
    }

    /// Takes one complete line, if the buffer holds one.
    ///
    /// A trailing `\r` waits for the next chunk: it may start a CRLF, and a
    /// spurious empty line would dispatch early.
    fn take_line(&mut self) -> Option<Vec<u8>> {
        let at = self
            .pending
            .iter()
            .position(|byte| matches!(byte, b'\n' | b'\r'))?;

        let is_cr = self.pending[at] == b'\r';
        if is_cr && at + 1 == self.pending.len() {
            return None;
        }

        let skip = usize::from(is_cr && self.pending.get(at + 1) == Some(&b'\n'));
        let rest = self.pending.split_off(at + 1 + skip);
        let mut line = std::mem::replace(&mut self.pending, rest);
        line.truncate(at);

        Some(line)
    }

    /// Applies one line, returning a payload when the line dispatched one.
    fn absorb_line(&mut self, line: &[u8]) -> Option<String> {
        if line.is_empty() {
            // The blank line is the dispatch. Anything accumulated goes out;
            // an event with no `data` field at all sends nothing.
            if !self.saw_data {
                return None;
            }
            self.saw_data = false;
            // The grammar joins several `data:` lines with newlines and drops
            // the last one.
            let mut payload = std::mem::take(&mut self.data);
            payload.pop();
            return Some(payload);
        }

        // Lossy on purpose: a byte sequence that is not UTF-8 at this point is
        // a broken frame, and mangling one line beats abandoning the reply.
        let line = String::from_utf8_lossy(line);
        let (field, value) = match line.split_once(':') {
            // `: comment`, including the keep-alives many servers send.
            Some(("", _)) => return None,
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            // A bare field name with no colon and no value.
            None => (line.as_ref(), ""),
        };

        if field == "data" {
            self.data.push_str(value);
            self.data.push('\n');
            self.saw_data = true;
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Joins a normalized base URL and a path into an endpoint.
///
/// Concatenation, not [`Url::join`], which would drop the `/v1`.
fn endpoint(base_url: &str, path: &str) -> Result<Url, String> {
    let trimmed = base_url.trim().trim_end_matches('/');

    Url::parse(&format!("{trimmed}{path}")).map_err(|err| {
        tracing::warn!(%err, base_url = trimmed, "the configured base URL is not a URL");
        format!("`{trimmed}` is not a usable base URL. Fix it in Settings.")
    })
}

/// The `Authorization` header, marked so it cannot be printed.
///
/// `set_sensitive` keeps it out of a `Debug` of the headers.
fn authorization(key: &ApiKey) -> Result<HeaderValue, String> {
    let mut header = HeaderValue::from_str(&format!("Bearer {}", key.expose())).map_err(|_| {
        // The message must not quote the key, and the failure is about its
        // shape rather than its value.
        "The API key contains characters that cannot be sent in a header. Re-copy it and save it \
         again."
            .to_owned()
    })?;

    header.set_sensitive(true);
    Ok(header)
}

/// What went wrong below HTTP, in words a user can act on.
///
/// Three cases, each with a different fix.
fn transport_reason(err: &reqwest::Error) -> String {
    if err.is_timeout() {
        "The server did not answer in time".to_owned()
    } else if err.is_body() || err.is_decode() {
        "The server's answer could not be read".to_owned()
    } else if err.is_connect() || io_cause(err).is_some() {
        // A refused connection may surface as an I/O error rather than
        // `is_connect`, depending on platform.
        "Aegis could not reach the server — check the base URL, and whether the server is running"
            .to_owned()
    } else {
        "The request failed".to_owned()
    }
}

/// The lowest-level I/O failure behind a request error, if there is one.
///
/// Walks the whole source chain, since `reqwest` nests causes unpredictably.
fn io_cause(err: &reqwest::Error) -> Option<&std::io::Error> {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(err);

    while let Some(current) = source {
        if let Some(io) = current.downcast_ref::<std::io::Error>() {
            return Some(io);
        }
        source = current.source();
    }
    None
}

/// One sentence for a failure status.
fn http_reason(status: StatusCode) -> String {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            format!("The server rejected the API key ({status}).")
        }
        StatusCode::NOT_FOUND => format!(
            "The server has no chat endpoint at that address ({status}). The base URL usually \
             ends in `/v1`."
        ),
        StatusCode::TOO_MANY_REQUESTS => {
            format!("The server is rate limiting this key ({status}). Try again shortly.")
        }
        status if status.is_server_error() => {
            format!("The server failed while answering ({status}).")
        }
        status => format!("The server refused the request ({status})."),
    }
}

/// The server's own explanation, when the body carries one.
async fn error_detail(response: reqwest::Response) -> Option<String> {
    let body = response.text().await.ok()?;
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }

    let message = match serde_json::from_str::<Value>(trimmed) {
        Ok(json) => json
            .get("error")
            .map_or_else(|| trimmed.to_owned(), error_message),
        Err(_) => trimmed.to_owned(),
    };

    Some(truncate(&message, ERROR_BODY_CHARS))
}

/// The text of an API error object, whatever shape it came in.
fn error_message(error: &Value) -> String {
    error
        .get("message")
        .and_then(Value::as_str)
        .map_or_else(|| error.to_string(), str::to_owned)
}

/// Adds the server's own words to a sentence, when there are any.
fn join_detail(message: String, detail: Option<String>) -> String {
    match detail {
        Some(detail) => format!("{message} The server said: {detail}"),
        None => message,
    }
}

/// Shortens on a character boundary, marking that it did.
fn truncate(text: &str, max_chars: usize) -> String {
    let mut out: String = text.chars().take(max_chars).collect();
    if out.chars().count() < text.chars().count() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
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
            &[
                json!({ "error": { "message": "context length exceeded", "type": "invalid_request" } }),
            ],
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

        let ModelEvent::Error { code, message, .. } = stream.recv().await.expect("one event")
        else {
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
        };
        let provider =
            OpenAiProvider::new(Some(Client::new()), &broken, ApiKey::new("sk-test-1234"));

        let mut stream = provider.stream(ModelRequest {
            model: "m".to_owned(),
            messages: Vec::new(),
            tools: Vec::new(),
        });

        let ModelEvent::Error { code, message, .. } = stream.recv().await.expect("one event")
        else {
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
}
