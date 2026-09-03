//! Real-time event bus for broadcasting mutation events to subscribers.
//!
//! Events are published to an [`EventTransport`]; subscribers receive them via
//! an [`EventReceiver`] that mirrors the semantics of `tokio::sync::broadcast`
//! (including a `Lagged` error when a slow subscriber cannot keep up).
//!
//! The default transport is [`InProcessEventBus`] — a thin wrapper around
//! `tokio::sync::broadcast`. A Redis pub/sub transport is available behind
//! `#[cfg(feature = "redis")]` for multi-server deployments.
//!
//! The same two-variant shape (in-process default + Redis) also applies to
//! the user-invalidation stream via [`InvalidationTransport`].

mod coalesce;
mod factory;
mod in_process;
mod receiver;
#[cfg(feature = "redis")]
mod redis_transport;
mod sequence;
mod transport;
mod types;

pub use coalesce::{DrainOutcome, MAX_DRAIN, coalesce_events, drain_and_coalesce};
pub use factory::{create_event_transport, create_invalidation_transport};
pub use in_process::{InProcessEventBus, InProcessInvalidationBus};
pub use receiver::{EventReceiver, InvalidationReceiver, RecvError, TryRecvError};
pub use transport::{
    EventTransport, InvalidationTransport, SharedEventTransport, SharedInvalidationTransport,
};
pub use types::{
    EventOperation, EventTarget, EventUser, EventViewMeta, MutationEvent, MutationEventInput,
};

pub(crate) use sequence::{SequenceGen, stamp_event};

#[cfg(feature = "redis")]
pub(crate) use receiver::RemoteMessage;
