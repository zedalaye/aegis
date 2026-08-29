//! The agent loop.
//!
//! Everything between "the user pressed send" and "the transcript has a reply
//! in it" lives here, arranged so that each piece can be replaced without
//! touching the others:
//!
//! * [`wire`] — the request body and the normalized event stream. The only
//!   module that knows what OpenAI-compatible JSON looks like.
//! * [`transcript`] — stored messages into a request, including the repairs
//!   that keep a cancelled turn from bricking a session.
//! * [`provider`] — where events come from. A fake one now, an HTTP one in
//!   Phase 8, chosen behind one trait.
//! * [`event`] — the payloads the WebView listens for, and the sink the turn
//!   emits them through.
//! * [`registry`] — which sessions are running, and how to cancel them.
//! * [`turn`] — the state machine that drives all of it.
//!
//! The seam that matters is between [`turn`] and everything below it. The loop
//! consumes [`wire::ModelEvent`] and emits [`event::Event`]; it never sees an
//! HTTP status, an SSE frame or an `AppHandle`. That is what lets the whole of
//! Phase 5 be tested without a network or a window, and what will let Phase 8
//! swap the provider without reopening this file.

pub mod event;
pub mod provider;
pub mod registry;
pub mod transcript;
pub mod turn;
pub mod wire;

pub use event::{Event, EventSink, NullSink};
pub use provider::{FakeProvider, Provider};
pub use registry::TurnRegistry;
pub use turn::{Turn, TurnPlan, MAX_TOOL_ROUNDS};
pub use wire::{ModelEvent, ModelRequest, StopReason, Usage};
