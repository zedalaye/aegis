//! The protocol between the runtime and a model (PLAN 4.1).
//!
//! Two things live here, and the split is the point. [`ModelRequest`] and its
//! `WireMessage` family are the OpenAI-compatible request body — the only
//! place in Aegis that knows what that JSON looks like. [`ModelEvent`] is the
//! *normalized* stream coming back: whatever shape a provider's SSE frames
//! arrive in, they are turned into these four variants before they leave
//! `provider/`, so nothing downstream ever sees provider JSON.
//!
//! That normalization is what makes the fake provider and the real one
//! interchangeable. `agent/turn.rs` consumes [`ModelEvent`] and has no way to
//! tell which produced it.
//!
//! The third piece is [`ToolCallAssembler`]. Streamed tool calls arrive as
//! fragments — a name in one frame, a slice of the arguments string in the
//! next — and they must be accumulated per index and parsed exactly once, at
//! the end. Doing it here rather than in each provider means Phase 8 inherits
//! the assembly, including its failure behaviour: a call whose arguments never
//! became valid JSON is not executed, it is answered, so the model can correct
//! itself instead of the turn dying (PLAN 4.1).

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
/// Built by [`transcript`](super::transcript) from a session's messages; sent
/// by a [`Provider`](super::provider::Provider). Streaming is not a field
/// because it is not a choice: Aegis always streams.
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
    /// `tools` and `tool_choice` are omitted entirely when no tool is on
    /// offer: some OpenAI-compatible servers reject an empty `tools` array
    /// rather than treating it as "no tools", and an absent key is what every
    /// implementation agrees on.
    ///
    /// `stream_options.include_usage` **is** sent, and that is a correction of
    /// a Phase 8 guess rather than a new feature (PLAN 7.3, Phase 17). That
    /// phase left it out on the reasoning that some compatible servers reject
    /// the extension and that most volunteer usage on the final chunk anyway.
    /// The second half is false: a streaming endpoint generally sends a usage
    /// chunk *only* when asked, so every turn came back unmeasured, and the
    /// token counters this phase is judged on had nothing to count. The first
    /// half is a risk worth taking now — the field is near-universal, servers
    /// that do not know it almost always ignore unknown keys, and one that
    /// refuses fails loudly on the next message and by name under
    /// `settings_probe_provider`, which is a far better failure than a counter
    /// that silently reads zero forever.
    pub fn to_body(&self) -> Value {
        let mut body = json!({
            "model": self.model,
            "stream": true,
            "stream_options": { "include_usage": true },
            "messages": self.messages,
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
/// The variants serialize to exactly the four shapes PLAN 4.1 shows. An
/// assistant message that made tool calls carries `content: null`, which is
/// what the API expects — not an empty string.
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
    },
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
    /// The *whole* prompt, always — which is the one definition that means the
    /// same thing across providers and across a cache hit. Anthropic reports
    /// its own `input_tokens` net of both cache figures below, so the provider
    /// puts them back before filling this in; the Responses API counts them in
    /// already and does not. Getting that wrong would make a well-cached turn
    /// look like a cheap one instead of a cheaply-*served* one.
    #[ts(type = "number")]
    pub prompt_tokens: u64,
    /// Of `prompt_tokens`, how many were served out of the prompt cache — the
    /// ones that cost about a tenth of what they would have.
    #[ts(type = "number")]
    pub cache_read_tokens: u64,
    /// Of `prompt_tokens`, how many were written to the cache for a later turn
    /// to read, at a premium over the plain price.
    ///
    /// Anthropic only: providers whose caching is automatic charge nothing to
    /// write and so report nothing, which is a zero here rather than a gap.
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
    /// A turn is a sequence of requests, and what it spent is their sum. Every
    /// field adds, the cache figures included: they are each a share of the
    /// `prompt_tokens` in the same round, so their shares sum too.
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
        /// Opaque. Must be echoed on the `functionCall` part of the next
        /// request or Gemini 3 rejects the round. Other providers leave this
        /// `None`.
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
/// Fragments are keyed by `index`, which is the only field a provider is
/// guaranteed to repeat: the id and the name arrive once, on whichever
/// fragment the provider felt like putting them on. Parsing is deferred to
/// [`ToolCallAssembler::finish`], because a partial arguments string is not
/// valid JSON and trying it early would fail on every call.
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
    /// `id` and `name` are taken the first time they appear and not
    /// overwritten afterwards: a provider that repeats them repeats the same
    /// value, and a provider that sends a second, different one is confused —
    /// the first is what the earlier fragments belong to.
    pub fn push(&mut self, index: u32, id: Option<String>, name: Option<String>, args_delta: &str) {
        self.push_signed(index, id, name, args_delta, None);
    }

    /// [`push`] plus a Gemini thought signature, taken the first time it appears.
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
    /// Three things are repaired rather than refused, because each is a
    /// provider quirk rather than a model mistake:
    ///
    /// * a missing id becomes `call_<index>` — ids only have to be unique
    ///   within the response, and the index already is;
    /// * empty arguments become `{}`, which is what a zero-argument call
    ///   means and what several servers send for one;
    /// * a call with no name at all cannot be repaired, and becomes an `Err`
    ///   the model is told about.
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
                WireMessage::User {
                    content: "hello".to_owned(),
                },
                WireMessage::Assistant {
                    content: None,
                    tool_calls: vec![WireToolCall::new(
                        "call_1",
                        "fs_read",
                        r#"{"path":"src/main.rs"}"#,
                    )],
                },
                WireMessage::Tool {
                    tool_call_id: "call_1".to_owned(),
                    content: r#"{"ok":true}"#.to_owned(),
                },
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
