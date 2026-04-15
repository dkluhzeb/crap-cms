//! In-process event and invalidation transports backed by
//! `tokio::sync::broadcast`. Default when no shared transport (Redis) is
//! configured.

use tokio::sync::broadcast;

use crate::core::event::{
    EventReceiver, EventTransport, InvalidationReceiver, InvalidationTransport, MutationEvent,
    MutationEventInput, SequenceGen, stamp_event,
};

/// Capacity of the user-invalidation broadcast channel. Low-volume signalling
/// (user lock / delete), so 64 is plenty — subscribers that lag beyond that
/// are force-dropped by the subscribe loop via the same Lagged mechanism.
const USER_INVALIDATION_CAPACITY: usize = 64;

/// In-process `EventTransport` — wraps a `tokio::sync::broadcast::Sender`.
///
/// Clone is cheap (Arc internals in `broadcast::Sender` + sequence generator).
#[derive(Clone)]
pub struct InProcessEventBus {
    sender: broadcast::Sender<MutationEvent>,
    sequence: SequenceGen,
}

impl InProcessEventBus {
    /// Create a new in-process event bus with the given channel capacity.
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);

        Self {
            sender,
            sequence: SequenceGen::new(),
        }
    }
}

impl EventTransport for InProcessEventBus {
    fn publish(&self, input: MutationEventInput) -> Option<MutationEvent> {
        let event = stamp_event(input, self.sequence.next());

        match self.sender.send(event.clone()) {
            Ok(_) => Some(event),
            // No active receivers, or send failed. This is normal when no
            // live-update clients are connected.
            Err(_) => None,
        }
    }

    fn subscribe(&self) -> EventReceiver {
        EventReceiver::from_broadcast(self.sender.subscribe())
    }

    fn kind(&self) -> &'static str {
        "in_process"
    }
}

/// In-process invalidation bus — wraps a `broadcast::Sender<String>`.
#[derive(Clone)]
pub struct InProcessInvalidationBus {
    sender: broadcast::Sender<String>,
}

impl Default for InProcessInvalidationBus {
    fn default() -> Self {
        Self::new()
    }
}

impl InProcessInvalidationBus {
    /// Create a new bus with the internal fixed capacity.
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(USER_INVALIDATION_CAPACITY);

        Self { sender }
    }
}

impl InvalidationTransport for InProcessInvalidationBus {
    fn publish(&self, user_id: String) {
        // Silently ignore "no receivers" — normal when no live clients connected.
        let _ = self.sender.send(user_id);
    }

    fn subscribe(&self) -> InvalidationReceiver {
        InvalidationReceiver::from_broadcast(self.sender.subscribe())
    }

    fn kind(&self) -> &'static str {
        "in_process"
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::core::event::{EventOperation, EventTarget, EventUser, RecvError};
    use crate::core::{DocumentId, Slug};

    use super::*;

    const TEST_CAPACITY: usize = 16;

    fn sample_input() -> MutationEventInput {
        MutationEventInput {
            target: EventTarget::Collection,
            operation: EventOperation::Create,
            collection: Slug::new("posts"),
            document_id: DocumentId::new("id1"),
            data: HashMap::new(),
            edited_by: None,
        }
    }

    #[test]
    fn publish_with_no_subscribers_returns_none() {
        let bus = InProcessEventBus::new(TEST_CAPACITY);
        assert!(bus.publish(sample_input()).is_none());
    }

    #[test]
    fn publish_with_subscriber_returns_event() {
        let bus = InProcessEventBus::new(TEST_CAPACITY);
        let _rx = bus.subscribe();

        let event = bus
            .publish(MutationEventInput {
                target: EventTarget::Collection,
                operation: EventOperation::Create,
                collection: Slug::new("posts"),
                document_id: DocumentId::new("id1"),
                data: HashMap::new(),
                edited_by: Some(EventUser::new("u1", "test@example.com")),
            })
            .expect("should publish with subscriber");

        assert_eq!(event.collection, "posts");
        assert_eq!(event.document_id, "id1");
        assert_eq!(event.target, EventTarget::Collection);
        assert_eq!(event.operation, EventOperation::Create);
        assert!(event.edited_by.is_some());
        assert_eq!(event.edited_by.unwrap().email, "test@example.com");
    }

    #[test]
    fn sequence_increments_across_publishes() {
        let bus = InProcessEventBus::new(TEST_CAPACITY);
        let _rx = bus.subscribe();

        let e1 = bus.publish(sample_input()).unwrap();
        let e2 = bus.publish(sample_input()).unwrap();

        assert_eq!(e2.sequence, e1.sequence + 1);
    }

    #[tokio::test]
    async fn in_process_event_transport_roundtrip() {
        let bus = InProcessEventBus::new(TEST_CAPACITY);
        let mut rx = bus.subscribe();

        bus.publish(sample_input());

        let event = rx.recv().await.expect("receive event");
        assert_eq!(event.collection, "posts");
    }

    #[tokio::test]
    async fn in_process_event_transport_multiple_subscribers() {
        let bus = InProcessEventBus::new(TEST_CAPACITY);
        let mut rx1 = bus.subscribe();
        let mut rx2 = bus.subscribe();

        bus.publish(sample_input());

        let e1 = rx1.recv().await.expect("rx1 event");
        let e2 = rx2.recv().await.expect("rx2 event");
        assert_eq!(e1.sequence, e2.sequence);
    }

    #[tokio::test]
    async fn in_process_event_transport_lagged_signal() {
        let bus = InProcessEventBus::new(2);
        let mut rx = bus.subscribe();

        // Overflow the channel without consuming.
        for _ in 0..5 {
            bus.publish(sample_input());
        }

        match rx.recv().await {
            Err(RecvError::Lagged(_)) => {}
            other => panic!("expected Lagged, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn in_process_invalidation_transport_roundtrip() {
        let bus = InProcessInvalidationBus::new();
        let mut rx = bus.subscribe();

        bus.publish("user-123".to_string());

        let id = rx.recv().await.expect("receive id");
        assert_eq!(id, "user-123");
    }

    #[test]
    fn in_process_invalidation_no_receivers_is_ok() {
        let bus = InProcessInvalidationBus::new();
        // No panic — publish without receivers is a no-op.
        bus.publish("user-x".to_string());
    }

    #[test]
    fn kind_returns_in_process() {
        let evt = InProcessEventBus::new(4);
        let inv = InProcessInvalidationBus::new();
        assert_eq!(evt.kind(), "in_process");
        assert_eq!(inv.kind(), "in_process");
    }
}
