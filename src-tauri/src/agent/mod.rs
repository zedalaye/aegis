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
//! * [`provider`] — where events come from, chosen per turn behind one trait.
//! * [`event`] — the payloads the WebView listens for, and the sink the turn
//!   emits them through.
//! * [`registry`] — which sessions are running, and how to cancel them.
//! * [`turn`] — the state machine that drives all of it.
//!
//! [`turn`] consumes [`wire::ModelEvent`] and emits [`event::Event`], never
//! seeing HTTP or an `AppHandle`, so it runs in tests without network or window.

pub mod decision;
pub mod event;
pub mod guard;
pub mod provider;
pub mod registry;
pub mod transcript;
pub mod turn;
pub mod wire;

pub use event::{Event, EventSink, NullSink};
pub use provider::{
    FakeProvider, ModelCatalog, OpenAiProvider, Provider, ProviderProbe, SubscriptionProvider,
};
pub use registry::TurnRegistry;
pub use turn::{Standing, Turn, TurnPlan, Unattended, MAX_TOOL_ROUNDS};
pub use wire::{ModelEvent, ModelRequest, StopReason, Usage};
