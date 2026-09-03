//! Where model events come from.
//!
//! One trait, one method. A provider is handed a [`ModelRequest`] and answers
//! with a stream of already-normalized [`ModelEvent`]s; no provider-specific
//! JSON, HTTP status or SSE framing escapes this module tree (PLAN 4.1).
//!
//! [`Provider::stream`] is not `async`, and that is deliberate. It returns the
//! channel immediately and does the work behind it, so the turn loop can begin
//! selecting on cancellation before the first byte arrives — an `async fn`
//! returning a stream would leave the connection attempt itself uncancellable.
//! The stream is a plain [`mpsc::Receiver`] rather than a `Stream` impl for
//! the same reason it is enough: the loop consumes it with `recv().await`
//! inside a `select!`, which is exactly what cancellation needs and what a
//! `Stream` would have to be adapted back into.
//!
//! Three implementations. [`fake`] is what Phase 5 streams from and what a
//! fresh install still answers with; [`openai`] speaks SSE to an
//! OpenAI-compatible endpoint; [`motosan`] speaks each vendor's own dialect —
//! a Claude Code or Codex CLI login already on this machine, and an API key
//! aimed at Anthropic (Grok reuses [`openai`] after a token refresh). Because
//! the boundary is this trait, those paths do not reopen `agent/turn.rs`.
//!
//! Which of the two answers a turn is decided per turn, from settings, in
//! [`AppState::provider`](crate::state::AppState::provider). Nothing here is a
//! singleton: a roster of providers later is a different choice at that one
//! call site, not a change to this trait (PLAN 7.1).

pub mod catalog;
pub mod fake;
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
