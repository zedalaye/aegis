//! A provider with no model behind it.
//!
//! It exists so everything above it can be finished and exercised before there
//! is an API key anywhere near the project: the turn loop, the transcript, the
//! event plumbing, the session list, cancellation, and — from Phase 6 — the
//! approval gate. All of that is provider-shaped work, and none of it should
//! wait on a network client.
//!
//! Two modes:
//!
//! * **Improvised** ([`FakeProvider::new`]) — the reply is derived from the
//!   request. It quotes the user, names the workspace it was given and counts
//!   the tools it was offered, so a reply that streams into the window is
//!   evidence that [`transcript::build`](crate::agent::transcript::build)
//!   really did carry those things.
//! * **Scripted** ([`FakeProvider::scripted`]) — exact event sequences, one
//!   per turn, consumed in order. This is how a test drives a tool call, a
//!   truncated arguments string or a provider error through the loop, and how
//!   Phase 6 makes the model ask for an `fs_write` on demand.
//!
//! Tokens are emitted with a small delay so streaming is visibly streaming and
//! a cancel has something to interrupt. Tests use [`FakeProvider::instant`],
//! which sets the delay to zero.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::agent::wire::{ModelEvent, ModelRequest, StopReason, Usage, WireMessage};

use super::{Provider, STREAM_BUFFER};

/// The model id this provider reports.
pub const FAKE_MODEL: &str = "aegis-fake-1";

/// Delay between tokens in the improvised reply.
///
/// Fast enough not to be a wait, slow enough that a person can see text
/// arriving rather than appearing — and long enough that a cancel sent by a
/// human lands mid-stream, which is the thing being exercised.
const TOKEN_DELAY: Duration = Duration::from_millis(18);

/// A provider that answers without a model.
#[derive(Debug)]
pub struct FakeProvider {
    model: String,
    delay: Duration,
    /// Remaining scripted turns. Empty means improvise.
    script: Mutex<VecDeque<Vec<ModelEvent>>>,
}

impl Default for FakeProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeProvider {
    /// The improvising provider the application runs with.
    pub fn new() -> Self {
        Self {
            model: FAKE_MODEL.to_owned(),
            delay: TOKEN_DELAY,
            script: Mutex::new(VecDeque::new()),
        }
    }

    /// [`FakeProvider::new`] with no pacing, for tests.
    pub fn instant() -> Self {
        Self {
            delay: Duration::ZERO,
            ..Self::new()
        }
    }

    /// A provider that replays `turns`, one sequence per request.
    ///
    /// Once the script runs out it improvises, so a test that scripts a tool
    /// call does not also have to script the reply that follows the tool
    /// result.
    pub fn scripted(turns: Vec<Vec<ModelEvent>>) -> Self {
        Self {
            delay: Duration::ZERO,
            script: Mutex::new(turns.into()),
            ..Self::new()
        }
    }

    /// Takes the next scripted turn, if there is one.
    fn next_script(&self) -> Option<Vec<ModelEvent>> {
        self.script
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front()
    }
}

impl Provider for FakeProvider {
    fn model(&self) -> &str {
        &self.model
    }

    fn stream(&self, request: ModelRequest) -> mpsc::Receiver<ModelEvent> {
        let (tx, rx) = mpsc::channel(STREAM_BUFFER);

        let events = match self.next_script() {
            Some(scripted) => scripted,
            None => improvise(&request),
        };
        let delay = self.delay;

        tokio::spawn(async move {
            for event in events {
                // A closed channel means the turn stopped listening — it was
                // cancelled, or the window went away. Producing tokens nobody
                // will read is the one thing a cancelled provider must not do.
                if tx.send(event).await.is_err() {
                    tracing::debug!("the fake provider's listener went away");
                    return;
                }
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
            }
        });

        rx
    }
}

/// Builds a reply out of the request itself.
///
/// Everything it says is measured from the request, so the text doubles as a
/// report on what the transcript actually sent: if the workspace line is wrong
/// or the tool count is zero, that is a real bug, visible in the window without
/// a debugger.
fn improvise(request: &ModelRequest) -> Vec<ModelEvent> {
    let said = last_user_text(request);
    let workspace = workspace_line(request);
    let tools = request.tools.len();

    let reply = format!(
        "You said: \u{201c}{said}\u{201d}\n\n\
         There is no model behind this build yet. This reply comes from the \
         scripted provider in `agent/provider/fake.rs`, streamed a token at a \
         time so the transcript, the session list and cancellation can all be \
         exercised end to end. Phase 8 replaces it with an OpenAI-compatible \
         client and nothing above this line changes.\n\n\
         {workspace}\n\
         I was offered {tools} tool{plural}, and I am not going to call any of \
         them — a fake model choosing to touch your filesystem would be a poor \
         joke.",
        plural = if tools == 1 { "" } else { "s" },
    );

    let mut events: Vec<ModelEvent> = tokens(&reply)
        .into_iter()
        .map(|text| ModelEvent::TextDelta { text })
        .collect();

    // Counted rather than invented: a usage figure that looked plausible would
    // be indistinguishable from a real one in the UI, and this one is honestly
    // a token count of a string nobody was charged for.
    let completion = u64::try_from(events.len()).unwrap_or(u64::MAX);
    let prompt = u64::try_from(request.messages.len()).unwrap_or(u64::MAX);

    events.push(ModelEvent::Finish {
        reason: StopReason::Stop,
        usage: Some(Usage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt.saturating_add(completion),
        }),
    });
    events
}

/// The last thing the user said, or a stand-in when they said nothing.
fn last_user_text(request: &ModelRequest) -> String {
    request
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            WireMessage::User { content } => Some(content.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "(nothing)".to_owned())
}

/// One line reporting the workspace the system message named.
///
/// Read back out of the built request rather than passed in separately, so it
/// reflects what was actually sent.
fn workspace_line(request: &ModelRequest) -> String {
    let system = request.messages.iter().find_map(|message| match message {
        WireMessage::System { content } => Some(content.as_str()),
        _ => None,
    });

    match system {
        Some(content) => match content
            .lines()
            .find_map(|line| line.strip_prefix("The workspace is: "))
        {
            Some(path) => format!("The workspace I was given is {path}."),
            None => "I was given no workspace, so no tool could run anyway.".to_owned(),
        },
        None => "I was sent no system message at all, which is a bug.".to_owned(),
    }
}

/// Splits text into streaming tokens.
///
/// Whitespace stays attached to the word before it, so concatenating every
/// token reproduces the input exactly — the property the transcript depends on
/// when it replaces the streamed buffer with the finalized message.
fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_gap = false;

    for ch in text.chars() {
        let is_space = ch.is_whitespace();

        // A word boundary is the first non-space after a run of spaces.
        if !is_space && in_gap && !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
        current.push(ch);
        in_gap = is_space;
    }

    if !current.is_empty() {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::agent::transcript;
    use crate::agent::wire::WireMessage;

    use std::path::PathBuf;

    /// Collects a whole stream.
    async fn drain(provider: &FakeProvider, request: ModelRequest) -> Vec<ModelEvent> {
        let mut rx = provider.stream(request);
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        events
    }

    /// The text of every `TextDelta`, concatenated.
    fn text_of(events: &[ModelEvent]) -> String {
        events
            .iter()
            .filter_map(|event| match event {
                ModelEvent::TextDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    fn request_saying(text: &str) -> ModelRequest {
        transcript::build(
            FAKE_MODEL,
            &[crate::store::Message::user(text)],
            Some(&PathBuf::from("/home/p/work")),
            crate::tools::schemas(),
        )
    }

    #[tokio::test]
    async fn a_reply_streams_and_then_finishes() {
        let events = drain(&FakeProvider::instant(), request_saying("hello there")).await;

        assert!(events.len() > 5, "the reply is streamed, not sent whole");
        assert!(matches!(
            events.last(),
            Some(ModelEvent::Finish {
                reason: StopReason::Stop,
                usage: Some(_)
            })
        ));
    }

    /// The reply is evidence about the request: if the transcript stopped
    /// carrying the workspace or the tool schemas, the text says so.
    #[tokio::test]
    async fn the_reply_reports_what_the_request_carried() {
        let events = drain(&FakeProvider::instant(), request_saying("hello there")).await;
        let text = text_of(&events);

        assert!(text.contains("hello there"), "{text}");
        assert!(text.contains("/home/p/work"), "{text}");
        assert!(
            text.contains(&format!("{} tools", crate::tools::schemas().len())),
            "{text}"
        );
    }

    #[tokio::test]
    async fn a_request_without_a_workspace_is_reported_as_such() {
        let request = transcript::build(FAKE_MODEL, &[], None, Vec::new());
        let text = text_of(&drain(&FakeProvider::instant(), request).await);

        assert!(text.contains("no workspace"), "{text}");
    }

    #[tokio::test]
    async fn a_script_is_replayed_verbatim_and_then_exhausts() {
        let scripted = vec![vec![
            ModelEvent::TextDelta {
                text: "one".to_owned(),
            },
            ModelEvent::Finish {
                reason: StopReason::ToolCalls,
                usage: None,
            },
        ]];
        let provider = FakeProvider::scripted(scripted.clone());

        assert_eq!(
            drain(&provider, request_saying("go")).await,
            scripted[0],
            "the first turn is exactly the script"
        );

        // The second turn has no script left, so it improvises rather than
        // returning nothing.
        let second = drain(&provider, request_saying("and again")).await;
        assert!(second.len() > 1);
        assert!(text_of(&second).contains("and again"));
    }

    /// Concatenating the tokens must reproduce the text exactly: the UI
    /// appends deltas to a buffer and then swaps in the finalized message, and
    /// the two have to agree.
    #[test]
    fn tokens_reassemble_into_the_original_text() {
        for text in [
            "hello there friend",
            "  leading and trailing  ",
            "line one\n\nline two",
            "one",
            "",
            "\u{201c}quoted\u{201d} and punctuated.",
        ] {
            assert_eq!(
                tokens(text).concat(),
                text,
                "round trip failed for {text:?}"
            );
        }
    }

    #[test]
    fn tokens_break_on_words_rather_than_characters() {
        assert_eq!(tokens("a bc  d"), vec!["a ", "bc  ", "d"]);
    }

    /// A dropped receiver is how the turn loop says it stopped listening. The
    /// provider must not keep producing into a channel nobody reads.
    #[tokio::test]
    async fn a_dropped_listener_stops_the_stream() {
        let provider = FakeProvider::new();
        let rx = provider.stream(request_saying("a long enough message to stream"));
        drop(rx);

        // Nothing to assert but the absence of a panic: the send fails, the
        // task returns. Yielding gives it the chance to do so under the test
        // runtime.
        tokio::task::yield_now().await;
    }

    #[tokio::test]
    async fn the_model_id_is_what_the_turn_reports() {
        let provider = FakeProvider::new();
        assert_eq!(provider.model(), FAKE_MODEL);

        let request = request_saying("x");
        assert_eq!(request.model, FAKE_MODEL);
        assert!(matches!(request.messages[0], WireMessage::System { .. }));
    }
}
