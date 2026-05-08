//! Mutation event payload types.

use serde::{Deserialize, Serialize};

use crate::core::{DocumentFields, DocumentId, Slug};

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
    pub data: DocumentFields,
    /// The user who performed the action, if known.
    pub edited_by: Option<EventUser>,
}

/// Inputs required to publish a mutation event. The transport fills in the
/// monotonic sequence number and ISO 8601 timestamp.
pub struct MutationEventInput {
    pub target: EventTarget,
    pub operation: EventOperation,
    pub collection: Slug,
    pub document_id: DocumentId,
    pub data: DocumentFields,
    pub edited_by: Option<EventUser>,
}

#[cfg(test)]
mod tests {
    use super::*;

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
            data: DocumentFields::new(),
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
