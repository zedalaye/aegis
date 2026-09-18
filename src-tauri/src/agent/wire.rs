//! The protocol between the runtime and a model (PLAN 4.1).
//!
//! * [`ModelRequest`] / `WireMessage`: the OpenAI-compatible request body.
//! * [`ModelEvent`]: the normalized stream every provider produces, so the turn
//!   loop cannot tell providers apart.
//! * [`ToolCallAssembler`]: accumulates streamed call fragments per index and
//!   parses once at the end; unparseable arguments are answered with an error,
//!   not executed (PLAN 4.1).
//! * [`WireImage`]: pixels for a user or tool message (PLAN 7.20). The
//!   transcript names a file; the provider reads it and each dialect writes
//!   it. Never an IPC payload.

use std::path::PathBuf;

use serde::Serialize;
use serde_json::{json, Value};
use ts_rs::TS;

/// Value of the `type` field on a tool call. The API has only this one.
const FUNCTION: &str = "function";

// ---------------------------------------------------------------------------
// Runtime → model
// ---------------------------------------------------------------------------

/// One request to a model.
///
/// Built by [`transcript`](super::transcript), sent by a
/// [`Provider`](super::provider::Provider). Always streamed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRequest {
    /// The model id, from settings (Phase 8) or the fake provider's own name.
    pub model: String,
    /// The conversation, system message first.
    pub messages: Vec<WireMessage>,
    /// The `tools` array, from [`tools::schemas`](crate::tools::schemas).
    pub tools: Vec<Value>,
}

impl ModelRequest {
    /// The HTTP request body of PLAN 4.1.
    ///
    /// `tools` and `tool_choice` are omitted when empty (some servers reject an
    /// empty array). `stream_options.include_usage` is sent (Phase 17): most
    /// servers only report usage when asked, and a refusing server fails
    /// visibly in the probe.
    pub fn to_body(&self) -> Value {
        let mut body = json!({
            "model": self.model,
            "stream": true,
            "stream_options": { "include_usage": true },
            "messages": openai_messages(&self.messages),
        });

        if !self.tools.is_empty() {
            // The insert cannot fail: `body` was just built as an object.
            if let Some(object) = body.as_object_mut() {
                object.insert("tools".to_owned(), json!(self.tools));
                object.insert("tool_choice".to_owned(), json!("auto"));
            }
        }
        body
    }
}

/// One message in the request body.
///
/// The four shapes of PLAN 4.1; an assistant message with tool calls has
/// `content: null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum WireMessage {
    /// The instructions, rebuilt for every request.
    System {
        /// The prompt text.
        content: String,
    },
    /// What the person typed.
    User {
        /// The message text.
        content: String,
        /// Images the person attached (PLAN 7.20); each dialect writes them.
        #[serde(skip)]
        images: Vec<WireImage>,
    },
    /// What the model said, and what it asked to run.
    Assistant {
        /// The text, or `null` for a message that was only tool calls.
        content: Option<String>,
        /// The calls. Omitted when there are none.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<WireToolCall>,
    },
    /// A tool result, answering one call.
    Tool {
        /// The call this answers.
        tool_call_id: String,
        /// The `ToolResult` envelope, as JSON text.
        content: String,
        /// Pixels the call produced — a capture (PLAN 7.20). The envelope
        /// stays text; dialects that cannot put an image in a tool result send
        /// it in a user turn right after the results.
        #[serde(skip)]
        images: Vec<WireImage>,
    },
}

impl WireMessage {
    /// A user message with no image.
    pub fn user(content: impl Into<String>) -> Self {
        Self::User {
            content: content.into(),
            images: Vec::new(),
        }
    }

    /// A tool result with no image.
    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::Tool {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
            images: Vec::new(),
        }
    }
}

/// An image on a request (PLAN 7.20).
///
/// Built as [`WireImage::File`] by the transcript, which reads no file; a
/// provider turns it into [`WireImage::Inline`] or [`WireImage::Missing`] with
/// [`image::load`](super::provider::image::load) before writing its body. A
/// missing image never fails the turn: the text goes without it, and says so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireImage {
    /// A file under the app's capture or attachment directory, not read yet.
    File {
        /// Absolute path.
        path: PathBuf,
    },
    /// Read, fitted to what providers take, and base64-encoded.
    Inline {
        /// `image/png`, `image/jpeg`, …
        mime: String,
        /// Standard base64, no line breaks.
        data: String,
    },
    /// Not sent. The reason is told to the model as text.
    Missing {
        /// Where the image was.
        path: PathBuf,
        /// Why it was not sent.
        reason: String,
    },
}

impl WireImage {
    /// What the model is told in place of an image it does not get, or `None`
    /// for one that is sent.
    pub fn note(&self) -> Option<String> {
        match self {
            Self::Inline { .. } => None,
            Self::File { path } => Some(format!(
                "[The image at `{}` was not sent: it was not read.]",
                path.display()
            )),
            Self::Missing { path, reason } => Some(format!(
                "[The image at `{}` was not sent: {reason}.]",
                path.display()
            )),
        }
    }

    /// The image as a data URL, when it is sent.
    fn data_url(&self) -> Option<String> {
        match self {
            Self::Inline { mime, data } => Some(format!("data:{mime};base64,{data}")),
            Self::File { .. } | Self::Missing { .. } => None,
        }
    }
}

/// `text` followed by the notes for the `images` that are not sent.
pub fn with_notes(text: &str, images: &[WireImage]) -> String {
    let mut out = text.to_owned();
    for note in images.iter().filter_map(WireImage::note) {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&note);
    }
    out
}

/// The lead-in of the user turn that carries a round's tool images, for the
/// dialects that cannot put an image inside a tool result.
pub const TOOL_IMAGES_LEAD: &str = "The image returned by the tool call above.";

/// The OpenAI-compatible `messages` array (PLAN 4.1, 7.20).
///
/// A user message with images becomes `content` parts. A tool message cannot
/// carry one, so a round's tool images follow its results as one user message.
fn openai_messages(messages: &[WireMessage]) -> Vec<Value> {
    let mut out = Vec::with_capacity(messages.len());
    let mut pending: Vec<Value> = Vec::new();

    for message in messages {
        if !matches!(message, WireMessage::Tool { .. }) && !pending.is_empty() {
            out.push(tool_images_turn(&mut pending));
        }
        match message {
            WireMessage::User { content, images } => {
                let text = with_notes(content, images);
                let parts: Vec<Value> = images.iter().filter_map(image_part).collect();
                if parts.is_empty() {
                    out.push(json!({ "role": "user", "content": text }));
                } else {
                    let mut content = Vec::with_capacity(parts.len() + 1);
                    if !text.is_empty() {
                        content.push(json!({ "type": "text", "text": text }));
                    }
                    content.extend(parts);
                    out.push(json!({ "role": "user", "content": content }));
                }
            }
            WireMessage::Tool {
                tool_call_id,
                content,
                images,
            } => {
                out.push(json!({
                    "role": "tool",
                    "tool_call_id": tool_call_id,
                    "content": with_notes(content, images),
                }));
                pending.extend(images.iter().filter_map(image_part));
            }
            // A derived `Serialize` into a `Value` cannot fail.
            other => out.push(serde_json::to_value(other).unwrap_or(Value::Null)),
        }
    }
    if !pending.is_empty() {
        out.push(tool_images_turn(&mut pending));
    }
    out
}

fn image_part(image: &WireImage) -> Option<Value> {
    image
        .data_url()
        .map(|url| json!({ "type": "image_url", "image_url": { "url": url } }))
}

fn tool_images_turn(pending: &mut Vec<Value>) -> Value {
    let mut content = vec![json!({ "type": "text", "text": TOOL_IMAGES_LEAD })];
    content.append(pending);
    json!({ "role": "user", "content": content })
}

/// One tool call in an assistant message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WireToolCall {
    /// The model's own id.
    pub id: String,
    /// Always `"function"`.
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// The call itself.
    pub function: WireFunction,
    /// Gemini thought signature to echo on the next request. Skipped on the
    /// OpenAI-compatible body: that dialect has no such field.
    #[serde(skip)]
    pub thought_signature: Option<String>,
}

impl WireToolCall {
    /// A call to `name` with `arguments` as a JSON string.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            kind: FUNCTION,
            function: WireFunction {
                name: name.into(),
                arguments: arguments.into(),
            },
            thought_signature: None,
        }
    }
}

/// The name and arguments of a call. Arguments are a JSON *string*, not an
/// object — that is the API's shape, and it is why they can arrive in pieces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WireFunction {
    /// The tool name.
    pub name: String,
    /// The arguments, as JSON text.
    pub arguments: String,
}

// ---------------------------------------------------------------------------
// Model → runtime
// ---------------------------------------------------------------------------

/// Why a model stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum StopReason {
    /// It finished what it was saying.
    Stop,
    /// It wants to run tools; the turn loops.
    ToolCalls,
    /// The user cancelled.
    Cancelled,
    /// It hit the model's own output ceiling.
    Length,
    /// The turn ended on an error. `turn:error` carries the detail.
    Error,
}

/// What a turn cost, when the provider says.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings.ts")]
pub struct Usage {
    /// Tokens in the request, whether or not they were paid for at full price.
    ///
    /// Always the whole prompt: Anthropic's net `input_tokens` get the cache
    /// figures added back.
    #[ts(type = "number")]
    pub prompt_tokens: u64,
    /// Of `prompt_tokens`, how many were served out of the prompt cache — the
    /// ones that cost about a tenth of what they would have.
    #[ts(type = "number")]
    pub cache_read_tokens: u64,
    /// Of `prompt_tokens`, how many were written to the cache for a later turn
    /// to read, at a premium over the plain price.
    ///
    /// Anthropic only; zero elsewhere.
    #[ts(type = "number")]
    pub cache_creation_tokens: u64,
    /// Tokens in the reply.
    #[ts(type = "number")]
    pub completion_tokens: u64,
    /// Their sum, as the provider reported it.
    #[ts(type = "number")]
    pub total_tokens: u64,
}

impl Usage {
    /// Adds another round's usage into this one.
    ///
    /// Every field sums, cache shares included.
    pub const fn add(&mut self, other: Self) {
        self.prompt_tokens = self.prompt_tokens.saturating_add(other.prompt_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(other.cache_read_tokens);
        self.cache_creation_tokens = self
            .cache_creation_tokens
            .saturating_add(other.cache_creation_tokens);
        self.completion_tokens = self
            .completion_tokens
            .saturating_add(other.completion_tokens);
        self.total_tokens = self.total_tokens.saturating_add(other.total_tokens);
    }
}

/// One normalized event from a provider's stream.
///
/// No provider JSON reaches past this type (PLAN 4.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelEvent {
    /// More assistant text.
    TextDelta {
        /// The fragment. Never trimmed — whitespace is content.
        text: String,
    },
    /// A fragment of a tool call.
    ToolCallDelta {
        /// Which call, within this response. Fragments accumulate per index.
        index: u32,
        /// The call id, sent once, usually on the first fragment.
        id: Option<String>,
        /// The tool name, sent once.
        name: Option<String>,
        /// A slice of the arguments JSON string.
        args_delta: String,
        /// Gemini thought signature for this call, when the part carried one.
        ///
        /// Opaque; must be echoed on the next request or Gemini 3 rejects it.
        thought_signature: Option<String>,
    },
    /// The response ended.
    Finish {
        /// Why.
        reason: StopReason,
        /// What it cost, when reported.
        usage: Option<Usage>,
    },
    /// The provider itself failed — HTTP, transport, or unparseable frames.
    Error {
        /// A stable code from PLAN 4.4.
        code: String,
        /// What went wrong, for the user.
        message: String,
        /// Whether an identical retry could work.
        retryable: bool,
    },
}

// ---------------------------------------------------------------------------
// Tool-call assembly
// ---------------------------------------------------------------------------

/// A tool call, accumulated and parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssembledCall {
    /// The id the model used, or one synthesized from the index.
    pub call_id: String,
    /// The tool name.
    pub name: String,
    /// The arguments exactly as they arrived, for the transcript.
    pub args_json: String,
    /// The parsed arguments, or why they could not be parsed.
    ///
    /// An `Err` is answered rather than executed: it becomes a `tool` message
    /// carrying an error envelope, and the turn continues (PLAN 4.1).
    pub args: Result<Value, String>,
    /// Gemini thought signature, when the stream carried one.
    pub thought_signature: Option<String>,
}

/// Accumulates streamed [`ModelEvent::ToolCallDelta`] fragments.
///
/// Keyed by `index`, the only field repeated on every fragment; parsed only in
/// [`ToolCallAssembler::finish`].
#[derive(Debug, Default)]
pub struct ToolCallAssembler {
    calls: Vec<Partial>,
}

/// One call, mid-assembly.
#[derive(Debug)]
struct Partial {
    index: u32,
    id: Option<String>,
    name: Option<String>,
    args: String,
    thought_signature: Option<String>,
}

impl ToolCallAssembler {
    /// Folds one fragment in.
    ///
    /// `id` and `name` are kept from their first appearance.
    pub fn push(&mut self, index: u32, id: Option<String>, name: Option<String>, args_delta: &str) {
        self.push_signed(index, id, name, args_delta, None);
    }

    /// [`Self::push`] plus a Gemini thought signature, kept from its first
    /// appearance.
    pub fn push_signed(
        &mut self,
        index: u32,
        id: Option<String>,
        name: Option<String>,
        args_delta: &str,
        thought_signature: Option<String>,
    ) {
        let partial = match self.calls.iter_mut().find(|call| call.index == index) {
            Some(existing) => existing,
            None => {
                self.calls.push(Partial {
                    index,
                    id: None,
                    name: None,
                    args: String::new(),
                    thought_signature: None,
                });
                // Just pushed, so this cannot be `None`.
                let Some(fresh) = self.calls.last_mut() else {
                    return;
                };
                fresh
            }
        };

        if partial.id.is_none() {
            partial.id = id;
        }
        if partial.name.is_none() {
            partial.name = name;
        }
        if partial.thought_signature.is_none() {
            partial.thought_signature = thought_signature.filter(|sig| !sig.is_empty());
        }
        partial.args.push_str(args_delta);
    }

    /// Whether any fragment has arrived.
    pub fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }

    /// Parses everything accumulated, in the order the model produced it.
    ///
    /// A missing id becomes `call_<index>` and empty arguments become `{}`; a
    /// missing name becomes an `Err` the model is told about.
    pub fn finish(mut self) -> Vec<AssembledCall> {
        self.calls.sort_by_key(|call| call.index);

        self.calls
            .into_iter()
            .map(|partial| {
                let call_id = partial
                    .id
                    .unwrap_or_else(|| format!("call_{}", partial.index));
                let raw = partial.args.trim();
                let args_json = if raw.is_empty() { "{}" } else { raw }.to_owned();

                let args = match partial.name.as_deref() {
                    None | Some("") => {
                        Err("the provider streamed a tool call with no function name".to_owned())
                    }
                    Some(_) => serde_json::from_str::<Value>(&args_json)
                        .map_err(|err| format!("the arguments are not valid JSON: {err}")),
                };

                AssembledCall {
                    call_id,
                    name: partial.name.unwrap_or_default(),
                    args_json,
                    args,
                    thought_signature: partial.thought_signature,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_body_matches_the_documented_shape() {
        let request = ModelRequest {
            model: "gpt-4o-mini".to_owned(),
            messages: vec![
                WireMessage::System {
                    content: "rules".to_owned(),
                },
                WireMessage::user("hello"),
                WireMessage::Assistant {
                    content: None,
                    tool_calls: vec![WireToolCall::new(
                        "call_1",
                        "fs_read",
                        r#"{"path":"src/main.rs"}"#,
                    )],
                },
                WireMessage::tool("call_1", r#"{"ok":true}"#),
            ],
            tools: vec![json!({ "type": "function" })],
        };

        let body = request.to_body();

        assert_eq!(body["model"], "gpt-4o-mini");
        assert_eq!(body["stream"], true);
        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(
            body["stream_options"]["include_usage"], true,
            "a streaming endpoint sends usage only when it is asked, and a \
             counter with nothing to count is what not asking produces"
        );

        let messages = &body["messages"];
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "rules");
        assert_eq!(messages[1]["role"], "user");

        assert_eq!(messages[2]["role"], "assistant");
        assert!(
            messages[2]["content"].is_null(),
            "a tool-call message carries a null content, not an empty string"
        );
        assert_eq!(messages[2]["tool_calls"][0]["id"], "call_1");
        assert_eq!(messages[2]["tool_calls"][0]["type"], "function");
        assert_eq!(messages[2]["tool_calls"][0]["function"]["name"], "fs_read");
        assert_eq!(
            messages[2]["tool_calls"][0]["function"]["arguments"], r#"{"path":"src/main.rs"}"#,
            "arguments cross the wire as a string, not as an object"
        );

        assert_eq!(messages[3]["role"], "tool");
        assert_eq!(messages[3]["tool_call_id"], "call_1");
    }

    fn inline() -> WireImage {
        WireImage::Inline {
            mime: "image/png".to_owned(),
            data: "iVBOR".to_owned(),
        }
    }

    #[test]
    fn a_user_image_becomes_a_content_part() {
        let body = ModelRequest {
            model: "m".to_owned(),
            messages: vec![WireMessage::User {
                content: "what is this?".to_owned(),
                images: vec![inline()],
            }],
            tools: Vec::new(),
        }
        .to_body();

        let content = &body["messages"][0]["content"];
        assert_eq!(
            content[0],
            json!({ "type": "text", "text": "what is this?" })
        );
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(
            content[1]["image_url"]["url"],
            "data:image/png;base64,iVBOR"
        );
    }

    #[test]
    fn tool_images_follow_the_round_as_one_user_turn() {
        let body = ModelRequest {
            model: "m".to_owned(),
            messages: vec![
                WireMessage::Assistant {
                    content: None,
                    tool_calls: vec![
                        WireToolCall::new("a", "screen_capture", "{}"),
                        WireToolCall::new("b", "fs_read", "{}"),
                    ],
                },
                WireMessage::Tool {
                    tool_call_id: "a".to_owned(),
                    content: "{}".to_owned(),
                    images: vec![inline()],
                },
                WireMessage::tool("b", "{}"),
            ],
            tools: Vec::new(),
        }
        .to_body();

        let messages = body["messages"].as_array().expect("messages");
        let roles: Vec<&str> = messages
            .iter()
            .map(|message| message["role"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(roles, ["assistant", "tool", "tool", "user"]);
        assert_eq!(messages[1]["content"], "{}", "the envelope stays text");
        assert_eq!(messages[3]["content"][0]["text"], TOOL_IMAGES_LEAD);
        assert_eq!(messages[3]["content"][1]["type"], "image_url");
    }

    #[test]
    fn a_missing_image_is_said_and_not_sent() {
        let body = ModelRequest {
            model: "m".to_owned(),
            messages: vec![WireMessage::User {
                content: "look".to_owned(),
                images: vec![WireImage::Missing {
                    path: PathBuf::from("/data/attachments/x.png"),
                    reason: "it is no longer on disk".to_owned(),
                }],
            }],
            tools: Vec::new(),
        }
        .to_body();

        let content = body["messages"][0]["content"].as_str().expect("plain text");
        assert!(content.starts_with("look\n\n[The image at"), "{content}");
        assert!(content.contains("it is no longer on disk"), "{content}");
    }

    #[test]
    fn a_plain_assistant_message_carries_no_tool_calls_key() {
        let body = ModelRequest {
            model: "m".to_owned(),
            messages: vec![WireMessage::Assistant {
                content: Some("hi".to_owned()),
                tool_calls: Vec::new(),
            }],
            tools: Vec::new(),
        }
        .to_body();

        assert_eq!(body["messages"][0]["content"], "hi");
        assert!(body["messages"][0].get("tool_calls").is_none());
    }

    /// An empty `tools` array is rejected outright by some OpenAI-compatible
    /// servers, so the key has to be absent rather than empty.
    #[test]
    fn a_request_without_tools_omits_the_key_entirely() {
        let body = ModelRequest {
            model: "m".to_owned(),
            messages: Vec::new(),
            tools: Vec::new(),
        }
        .to_body();

        assert!(body.get("tools").is_none());
        assert!(body.get("tool_choice").is_none());
    }

    #[test]
    fn fragments_accumulate_per_index() {
        let mut assembler = ToolCallAssembler::default();
        assert!(assembler.is_empty());

        // Interleaved, as a provider streaming two parallel calls sends them.
        assembler.push(0, Some("call_a".to_owned()), Some("fs_read".to_owned()), "");
        assembler.push(1, Some("call_b".to_owned()), Some("fs_list".to_owned()), "");
        assembler.push(0, None, None, r#"{"path":"#);
        assembler.push(1, None, None, r#"{"path":"."}"#);
        assembler.push(0, None, None, r#""a.txt"}"#);

        let calls = assembler.finish();
        assert_eq!(calls.len(), 2);

        assert_eq!(calls[0].call_id, "call_a");
        assert_eq!(calls[0].name, "fs_read");
        assert_eq!(calls[0].args_json, r#"{"path":"a.txt"}"#);
        assert_eq!(
            calls[0].args.as_ref().expect("parsed")["path"],
            json!("a.txt")
        );

        assert_eq!(calls[1].call_id, "call_b");
        assert_eq!(calls[1].name, "fs_list");
    }

    /// The index is the only field a provider must repeat, so a stream that
    /// only ever names the call once still has to assemble.
    #[test]
    fn an_id_and_a_name_are_taken_from_whichever_fragment_carries_them() {
        let mut assembler = ToolCallAssembler::default();
        assembler.push(0, None, None, "{");
        assembler.push(
            0,
            Some("call_x".to_owned()),
            Some("fs_list".to_owned()),
            r#""path":"."#,
        );
        assembler.push(0, None, None, r#""}"#);

        let calls = assembler.finish();
        assert_eq!(calls[0].call_id, "call_x");
        assert_eq!(calls[0].name, "fs_list");
        assert!(calls[0].args.is_ok(), "{:?}", calls[0].args);
    }

    #[test]
    fn a_call_with_no_id_is_named_after_its_index() {
        let mut assembler = ToolCallAssembler::default();
        assembler.push(3, None, Some("fs_list".to_owned()), r#"{"path":"."}"#);

        assert_eq!(assembler.finish()[0].call_id, "call_3");
    }

    #[test]
    fn empty_arguments_mean_an_empty_object() {
        let mut assembler = ToolCallAssembler::default();
        assembler.push(
            0,
            Some("c".to_owned()),
            Some("screen_capture".to_owned()),
            "",
        );

        let calls = assembler.finish();
        assert_eq!(calls[0].args_json, "{}");
        assert_eq!(calls[0].args.as_ref().expect("parsed"), &json!({}));
    }

    /// Truncated arguments are the common real failure — the model ran out of
    /// output mid-JSON. It must come back as an `Err` the turn can answer, not
    /// as something that gets executed with half its arguments.
    #[test]
    fn unparseable_arguments_are_reported_rather_than_executed() {
        let mut assembler = ToolCallAssembler::default();
        assembler.push(
            0,
            Some("c".to_owned()),
            Some("fs_write".to_owned()),
            r#"{"path":"a.txt","#,
        );

        let calls = assembler.finish();
        let err = calls[0].args.as_ref().expect_err("truncated JSON");
        assert!(err.contains("not valid JSON"), "{err}");
        assert_eq!(
            calls[0].args_json, r#"{"path":"a.txt","#,
            "the transcript keeps what actually arrived"
        );
    }

    #[test]
    fn a_call_with_no_name_cannot_be_run() {
        let mut assembler = ToolCallAssembler::default();
        assembler.push(0, Some("c".to_owned()), None, "{}");

        let calls = assembler.finish();
        assert!(calls[0]
            .args
            .as_ref()
            .expect_err("nameless")
            .contains("no function name"));
    }

    #[test]
    fn a_thought_signature_is_kept_with_the_call() {
        let mut assembler = ToolCallAssembler::default();
        assembler.push_signed(
            0,
            Some("call_0".to_owned()),
            Some("fs_list".to_owned()),
            "{}",
            Some("sig-1".to_owned()),
        );
        let calls = assembler.finish();
        assert_eq!(calls[0].thought_signature.as_deref(), Some("sig-1"));
    }

    #[test]
    fn stop_reasons_cross_the_wire_as_snake_case() {
        let json = serde_json::to_value(StopReason::ToolCalls).expect("serializes");
        assert_eq!(json, json!("tool_calls"));
    }
}
