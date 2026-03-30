//! Real-time event bus for broadcasting mutation events to subscribers.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use serde::Serialize;
use serde_json::Value;
use tokio::sync::broadcast;

use crate::core::{DocumentId, Slug};

/// The type of entity that was mutated.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum EventTarget {
    /// A collection document.
    Collection,
    /// A global setting.
    Global,
}

/// The mutation operation that occurred.
#[derive(Debug, Clone, Serialize, PartialEq)]
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
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
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

/// Broadcast channel for real-time mutation events.
/// Clone is cheap (Arc internals).
#[derive(Clone)]
pub struct EventBus {
    sender: broadcast::Sender<MutationEvent>,
    sequence: Arc<AtomicU64>,
}

impl EventBus {
    /// Create a new EventBus with the given channel capacity.
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self {
            sender,
            sequence: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Publish a mutation event to all subscribers.
    /// Assigns a monotonic sequence number and ISO 8601 timestamp.
    /// Returns the published event, or None if there are no receivers.
    pub fn publish(
        &self,
        target: EventTarget,
        operation: EventOperation,
        collection: Slug,
        document_id: DocumentId,
        data: HashMap<String, Value>,
        edited_by: Option<EventUser>,
    ) -> Option<MutationEvent> {
        let event = MutationEvent {
            sequence: self.sequence.fetch_add(1, Ordering::AcqRel),
            timestamp: chrono::Utc::now().to_rfc3339(),
            target,
            operation,
            collection,
            document_id,
            data,
            edited_by,
        };

        match self.sender.send(event.clone()) {
            Ok(_) => Some(event),
            Err(_) => None, // no active receivers
        }
    }

    /// Subscribe to the event stream. Returns a receiver that gets all
    /// future events. Missed events (due to slow consumption) result in
    /// `broadcast::error::RecvError::Lagged`.
    pub fn subscribe(&self) -> broadcast::Receiver<MutationEvent> {
        self.sender.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_creates_bus() {
        let bus = EventBus::new(16);
        // Just verify it doesn't panic
        let _rx = bus.subscribe();
    }

    #[test]
    fn publish_with_no_subscribers_returns_none() {
        let bus = EventBus::new(16);
        // No subscribe() called, so no receivers
        let result = bus.publish(
            EventTarget::Collection,
            EventOperation::Create,
            Slug::new("posts"),
            DocumentId::new("id1"),
            HashMap::new(),
            None,
        );
        assert!(result.is_none());
    }

    #[test]
    fn publish_with_subscriber_returns_event() {
        let bus = EventBus::new(16);
        let _rx = bus.subscribe(); // create a receiver
        let result = bus.publish(
            EventTarget::Collection,
            EventOperation::Create,
            Slug::new("posts"),
            DocumentId::new("id1"),
            HashMap::new(),
            Some(EventUser {
                id: "u1".into(),
                email: "test@example.com".into(),
            }),
        );
        assert!(result.is_some());
        let event = result.unwrap();
        assert_eq!(event.collection, "posts");
        assert_eq!(event.document_id, "id1");
        assert_eq!(event.target, EventTarget::Collection);
        assert_eq!(event.operation, EventOperation::Create);
        assert!(event.edited_by.is_some());
        assert_eq!(event.edited_by.unwrap().email, "test@example.com");
    }

    #[test]
    fn sequence_increments() {
        let bus = EventBus::new(16);
        let _rx = bus.subscribe();
        let e1 = bus
            .publish(
                EventTarget::Collection,
                EventOperation::Create,
                Slug::new("a"),
                DocumentId::new("1"),
                HashMap::new(),
                None,
            )
            .unwrap();
        let e2 = bus
            .publish(
                EventTarget::Collection,
                EventOperation::Update,
                Slug::new("a"),
                DocumentId::new("2"),
                HashMap::new(),
                None,
            )
            .unwrap();
        assert_eq!(e2.sequence, e1.sequence + 1);
    }

    /// Regression: publishing multiple events must produce monotonically
    /// increasing sequence numbers (verifies AcqRel ordering).
    #[test]
    fn publish_multiple_events_monotonic_sequence() {
        let bus = EventBus::new(64);
        let _rx = bus.subscribe();

        let mut sequences = Vec::new();
        for i in 0..10 {
            let event = bus
                .publish(
                    EventTarget::Collection,
                    EventOperation::Create,
                    Slug::new("items"),
                    DocumentId::new(format!("id{}", i)),
                    HashMap::new(),
                    None,
                )
                .expect("should publish with subscriber");
            sequences.push(event.sequence);
        }

        // Every sequence number must be strictly greater than the previous
        for window in sequences.windows(2) {
            assert!(
                window[1] > window[0],
                "sequence numbers must be strictly increasing: {} should be > {}",
                window[1],
                window[0]
            );
        }

        // First sequence should start at 1 (initial AtomicU64 value)
        assert_eq!(sequences[0], 1);
    }

    #[test]
    fn subscriber_receives_event() {
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe();
        bus.publish(
            EventTarget::Global,
            EventOperation::Update,
            Slug::new("settings"),
            DocumentId::new("default"),
            HashMap::new(),
            None,
        );
        let event = rx.try_recv().expect("should receive event");
        assert_eq!(event.collection, "settings");
        assert_eq!(event.target, EventTarget::Global);
    }
}
