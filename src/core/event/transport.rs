//! Transport traits — pluggable backends for the event and invalidation streams.

use std::sync::Arc;

use super::{
    receiver::{EventReceiver, InvalidationReceiver},
    types::{MutationEvent, MutationEventInput},
};

/// Thread-safe shared reference to an event transport.
pub type SharedEventTransport = Arc<dyn EventTransport>;

/// Thread-safe shared reference to an invalidation transport.
pub type SharedInvalidationTransport = Arc<dyn InvalidationTransport>;

/// Publish/subscribe transport for mutation events.
pub trait EventTransport: Send + Sync {
    /// Publish a mutation event. Returns the published event (with sequence and
    /// timestamp filled in) or `None` when the underlying transport dropped it
    /// (e.g. no subscribers on the in-process broadcast, or the Redis publish
    /// failed — Redis backend logs the error internally).
    fn publish(&self, input: MutationEventInput) -> Option<MutationEvent>;

    /// Subscribe to the event stream.
    fn subscribe(&self) -> EventReceiver;

    /// Backend identifier (`"in_process"`, `"redis"`) for diagnostics.
    fn kind(&self) -> &'static str;
}

/// Publish/subscribe transport for user-invalidation signals.
pub trait InvalidationTransport: Send + Sync {
    /// Publish an invalidation signal for the given user ID.
    fn publish(&self, user_id: String);

    /// Subscribe to the invalidation stream.
    fn subscribe(&self) -> InvalidationReceiver;

    /// Backend identifier for diagnostics.
    fn kind(&self) -> &'static str;
}
