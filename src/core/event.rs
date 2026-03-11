//! Real-time event bus for broadcasting mutation events to subscribers.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::broadcast;

/// The type of entity that was mutated.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum EventTarget {
    Collection,
    Global,
}

/// The mutation operation that occurred.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum EventOperation {
    Create,
    Update,
    Delete,
}

/// The user who triggered a mutation event.
#[derive(Debug, Clone, Serialize)]
pub struct EventUser {
    pub id: String,
    pub email: String,
}

impl EventUser {
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
    pub sequence: u64,
    pub timestamp: String,
    pub target: EventTarget,
    pub operation: EventOperation,
    pub collection: String,
    pub document_id: String,
    pub data: HashMap<String, serde_json::Value>,
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
        collection: String,
        document_id: String,
        data: HashMap<String, serde_json::Value>,
        edited_by: Option<EventUser>,
    ) -> Option<MutationEvent> {
        let event = MutationEvent {
            sequence: self.sequence.fetch_add(1, Ordering::Relaxed),
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
            "posts".to_string(),
            "id1".to_string(),
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
            "posts".to_string(),
            "id1".to_string(),
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
                "a".into(),
                "1".into(),
                HashMap::new(),
                None,
            )
            .unwrap();
        let e2 = bus
            .publish(
                EventTarget::Collection,
                EventOperation::Update,
                "a".into(),
                "2".into(),
                HashMap::new(),
                None,
            )
            .unwrap();
        assert_eq!(e2.sequence, e1.sequence + 1);
    }

    #[test]
    fn subscriber_receives_event() {
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe();
        bus.publish(
            EventTarget::Global,
            EventOperation::Update,
            "settings".into(),
            "default".into(),
            HashMap::new(),
            None,
        );
        let event = rx.try_recv().expect("should receive event");
        assert_eq!(event.collection, "settings");
        assert_eq!(event.target, EventTarget::Global);
    }
}
