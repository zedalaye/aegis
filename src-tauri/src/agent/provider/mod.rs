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
//! Two implementations are planned. [`fake`] is here now and is what Phase 5
//! streams from; `openai.rs` (Phase 8) speaks SSE to an OpenAI-compatible
//! endpoint. Because the boundary is this trait, swapping them changes nothing
//! in `agent/turn.rs`.

pub mod fake;

use tokio::sync::mpsc;

use super::wire::{ModelEvent, ModelRequest};

pub use fake::FakeProvider;

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
