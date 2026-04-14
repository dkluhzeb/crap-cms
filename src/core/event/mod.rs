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

mod factory;
mod in_process;
#[cfg(feature = "redis")]
mod redis_transport;

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{broadcast, mpsc};

use crate::core::{DocumentId, Slug};

pub use factory::{create_event_transport, create_invalidation_transport};
pub use in_process::{InProcessEventBus, InProcessInvalidationBus};

/// Thread-safe shared reference to an event transport.
pub type SharedEventTransport = Arc<dyn EventTransport>;

/// Thread-safe shared reference to an invalidation transport.
pub type SharedInvalidationTransport = Arc<dyn InvalidationTransport>;

/// The type of entity that was mutated.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum EventTarget {
    /// A collection document.
    Collection,
    /// A global setting.
    Global,
}

/// The mutation operation that occurred.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum EventOperation {
    /// A new document or global was created.
    Create,
    /// An existing document or global was updated.
    Update,
    /// A document was deleted.
    Delete,
}

/// The user who triggered a mutation event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventUser {
    /// The unique identifier of the user.
    pub id: String,
    /// The email address of the user.
    pub email: String,
}

impl EventUser {
    /// Create a new event user.
    pub fn new(id: impl Into<String>, email: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            email: email.into(),
        }
    }
}

/// A mutation event broadcast to all subscribers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationEvent {
    /// A monotonic sequence number for ordering events.
    pub sequence: u64,
    /// The ISO 8601 timestamp when the event occurred.
    pub timestamp: String,
    /// The type of target that was mutated.
    pub target: EventTarget,
    /// The type of operation performed.
    pub operation: EventOperation,
    /// The slug of the collection or global.
    pub collection: Slug,
    /// The ID of the document or global name.
    pub document_id: DocumentId,
    /// The data that was changed or the full state.
    pub data: HashMap<String, Value>,
    /// The user who performed the action, if known.
    pub edited_by: Option<EventUser>,
}

/// Reasons a receiver could fail to deliver the next message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvError {
    /// The subscriber fell behind — one or more events were dropped. The inner
    /// value is the number of events that were skipped.
    Lagged(u64),
    /// The transport has been closed; no further events will be delivered.
    Closed,
}

impl std::fmt::Display for RecvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecvError::Lagged(n) => write!(f, "receiver lagged by {} events", n),
            RecvError::Closed => write!(f, "receiver closed"),
        }
    }
}

impl std::error::Error for RecvError {}

impl From<broadcast::error::RecvError> for RecvError {
    fn from(err: broadcast::error::RecvError) -> Self {
        match err {
            broadcast::error::RecvError::Lagged(n) => RecvError::Lagged(n),
            broadcast::error::RecvError::Closed => RecvError::Closed,
        }
    }
}

/// Internal receiver backing for [`EventReceiver`] / [`InvalidationReceiver`].
///
/// In-process transports use the broadcast channel directly. Redis transports
/// use a bounded mpsc channel fed by a background task reading pub/sub; the
/// task signals `Lagged` by sending a dedicated sentinel variant.
enum RecvKind<T: Clone + Send + 'static> {
    Broadcast(broadcast::Receiver<T>),
    /// Only constructed on the Redis transport path. Feature-gated to avoid a
    /// dead-code warning in the default (in-process-only) build.
    #[cfg_attr(not(feature = "redis"), allow(dead_code))]
    Mpsc(mpsc::Receiver<RemoteMessage<T>>),
}

/// Message type carried over the mpsc channel from a remote-transport pump task
/// to a local subscriber. The task either delivers a payload, or signals that
/// the local bounded queue overflowed so the subscriber should be dropped with
/// the same `Lagged` semantic as in-process broadcasts.
#[derive(Clone)]
#[cfg_attr(not(feature = "redis"), allow(dead_code))]
pub(crate) enum RemoteMessage<T: Clone + Send + 'static> {
    Event(T),
    Lagged(u64),
}

/// A receiver for mutation events. Mirrors `broadcast::Receiver` semantics
/// (error on lag) regardless of whether the underlying transport is in-process
/// or Redis pub/sub.
pub struct EventReceiver {
    inner: RecvKind<MutationEvent>,
}

impl EventReceiver {
    pub(crate) fn from_broadcast(rx: broadcast::Receiver<MutationEvent>) -> Self {
        Self {
            inner: RecvKind::Broadcast(rx),
        }
    }

    #[cfg(feature = "redis")]
    pub(crate) fn from_mpsc(rx: mpsc::Receiver<RemoteMessage<MutationEvent>>) -> Self {
        Self {
            inner: RecvKind::Mpsc(rx),
        }
    }

    /// Await the next event. Returns `Err(RecvError::Lagged(n))` if the
    /// subscriber fell behind (same semantic as `broadcast::Receiver::recv`).
    pub async fn recv(&mut self) -> Result<MutationEvent, RecvError> {
        match &mut self.inner {
            RecvKind::Broadcast(rx) => rx.recv().await.map_err(RecvError::from),
            RecvKind::Mpsc(rx) => match rx.recv().await {
                Some(RemoteMessage::Event(ev)) => Ok(ev),
                Some(RemoteMessage::Lagged(n)) => Err(RecvError::Lagged(n)),
                None => Err(RecvError::Closed),
            },
        }
    }
}

/// A receiver for user-invalidation signals. Same shape as [`EventReceiver`],
/// payload is the user document ID string.
pub struct InvalidationReceiver {
    inner: RecvKind<String>,
}

impl InvalidationReceiver {
    pub(crate) fn from_broadcast(rx: broadcast::Receiver<String>) -> Self {
        Self {
            inner: RecvKind::Broadcast(rx),
        }
    }

    #[cfg(feature = "redis")]
    pub(crate) fn from_mpsc(rx: mpsc::Receiver<RemoteMessage<String>>) -> Self {
        Self {
            inner: RecvKind::Mpsc(rx),
        }
    }

    /// Await the next invalidation signal. Returns `Err(RecvError::Lagged(n))`
    /// if the subscriber fell behind.
    pub async fn recv(&mut self) -> Result<String, RecvError> {
        match &mut self.inner {
            RecvKind::Broadcast(rx) => rx.recv().await.map_err(RecvError::from),
            RecvKind::Mpsc(rx) => match rx.recv().await {
                Some(RemoteMessage::Event(s)) => Ok(s),
                Some(RemoteMessage::Lagged(n)) => Err(RecvError::Lagged(n)),
                None => Err(RecvError::Closed),
            },
        }
    }
}

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

/// Inputs required to publish a mutation event. The transport fills in the
/// monotonic sequence number and ISO 8601 timestamp.
pub struct MutationEventInput {
    pub target: EventTarget,
    pub operation: EventOperation,
    pub collection: Slug,
    pub document_id: DocumentId,
    pub data: HashMap<String, Value>,
    pub edited_by: Option<EventUser>,
}

/// Monotonic sequence generator shared between transports. Starts at 1.
#[derive(Clone)]
pub(crate) struct SequenceGen {
    counter: Arc<AtomicU64>,
}

impl SequenceGen {
    pub(crate) fn new() -> Self {
        Self {
            counter: Arc::new(AtomicU64::new(1)),
        }
    }

    pub(crate) fn next(&self) -> u64 {
        self.counter.fetch_add(1, Ordering::AcqRel)
    }
}

/// Build a [`MutationEvent`] from an input plus a fresh sequence number and
/// timestamp.
pub(crate) fn stamp_event(input: MutationEventInput, sequence: u64) -> MutationEvent {
    let MutationEventInput {
        target,
        operation,
        collection,
        document_id,
        data,
        edited_by,
    } = input;

    MutationEvent {
        sequence,
        timestamp: chrono::Utc::now().to_rfc3339(),
        target,
        operation,
        collection,
        document_id,
        data,
        edited_by,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recv_error_from_broadcast_lagged() {
        let err: RecvError = broadcast::error::RecvError::Lagged(7).into();
        assert_eq!(err, RecvError::Lagged(7));
    }

    #[test]
    fn recv_error_from_broadcast_closed() {
        let err: RecvError = broadcast::error::RecvError::Closed.into();
        assert_eq!(err, RecvError::Closed);
    }

    #[test]
    fn recv_error_display() {
        assert_eq!(
            RecvError::Lagged(3).to_string(),
            "receiver lagged by 3 events"
        );
        assert_eq!(RecvError::Closed.to_string(), "receiver closed");
    }

    #[test]
    fn sequence_gen_is_monotonic() {
        let seq = SequenceGen::new();
        assert_eq!(seq.next(), 1);
        assert_eq!(seq.next(), 2);
        assert_eq!(seq.next(), 3);
    }

    #[test]
    fn stamp_event_fills_sequence_and_timestamp() {
        let input = MutationEventInput {
            target: EventTarget::Collection,
            operation: EventOperation::Create,
            collection: Slug::new("posts"),
            document_id: DocumentId::new("id1"),
            data: HashMap::new(),
            edited_by: None,
        };
        let event = stamp_event(input, 42);
        assert_eq!(event.sequence, 42);
        assert!(!event.timestamp.is_empty());
    }

    #[test]
    fn mutation_event_roundtrips_through_json() {
        // Required for the Redis transport's JSON wire format.
        let event = MutationEvent {
            sequence: 5,
            timestamp: "2024-01-01T00:00:00Z".into(),
            target: EventTarget::Collection,
            operation: EventOperation::Update,
            collection: Slug::new("posts"),
            document_id: DocumentId::new("abc"),
            data: HashMap::new(),
            edited_by: Some(EventUser::new("u1", "u@example.com")),
        };
        let json = serde_json::to_string(&event).unwrap();
        let decoded: MutationEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.sequence, 5);
        assert_eq!(decoded.operation, EventOperation::Update);
        assert_eq!(decoded.target, EventTarget::Collection);
        assert_eq!(decoded.document_id, "abc");
        assert_eq!(decoded.edited_by.unwrap().email, "u@example.com");
    }
}
