//! Where model events come from.
//!
//! A [`ModelRequest`] in, normalized [`ModelEvent`]s out; no wire details leave
//! this tree (PLAN 4.1). [`Provider::stream`] is not `async` and returns an
//! [`mpsc::Receiver`] at once, so even the connection attempt is cancellable.
//!
//! [`fake`] (unconfigured installs), [`openai`] (OpenAI-compatible SSE), and
//! [`motosan`] (Claude Code/Codex logins, Anthropic keys, Gemini; Grok via
//! `openai`). Chosen per turn by
//! [`AppState::provider`](crate::state::AppState::provider).

pub mod catalog;
pub mod fake;
pub mod image;
pub mod motosan;
pub mod openai;

use tokio::sync::mpsc;

use super::wire::{ModelEvent, ModelRequest};

pub use catalog::ModelCatalog;
pub use fake::FakeProvider;
pub use motosan::SubscriptionProvider;
pub use openai::{OpenAiProvider, ProviderProbe};

/// How many events a provider may run ahead of the turn loop.
///
/// Small on purpose: the loop consumes as fast as it can, and a deep buffer
/// would only let a cancelled turn keep producing tokens nobody will read.
pub const STREAM_BUFFER: usize = 32;

/// A source of model events.
pub trait Provider: Send + Sync {
    /// The model id to report in `turn:started` and to send in the request.
    fn model(&self) -> &str;

    /// Starts a response and returns the stream of events it produces.
    ///
    /// Returns immediately; the work happens on a task feeding the channel.
    /// Dropping the receiver is how a caller says it has stopped listening —
    /// a provider whose send fails must stop rather than keep working.
    fn stream(&self, request: ModelRequest) -> mpsc::Receiver<ModelEvent>;
}
