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

/// View-scoping metadata carried on every mutation event so a subscriber can be
/// gated by their per-view access (`read`/`draft`/`trash`) independent of the
/// `live_mode` data stripping that empties `MutationEvent.data` for
/// metadata-only collections. Without it, default (`Metadata`-mode) and delete
/// events — which carry no `data` — could not be view-filtered, leaking the
/// existence of draft/trashed documents to subscribers lacking that view.
///
/// Travels over the Redis transport too (multi-server), so it derives the same
/// serde + clone surface as the rest of the event.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EventViewMeta {
    /// The document's `_status` ("published"/"draft") relevant to this event:
    /// post-mutation for create/update, pre-deletion for delete. `None` for
    /// collections without a status axis and for globals (gated by `read`).
    pub status: Option<String>,
    /// Whether the event concerns a trashed document — a soft-delete moves the
    /// row to trash. When true the event is gated by `trash` rather than the
    /// status axis (`read`/`draft`).
    pub trashed: bool,
}

impl EventViewMeta {
    /// Derive view metadata from a document's full fields (create/update path),
    /// read **before** `live_mode` stripping. `_status` names the status view;
    /// a non-null `_deleted_at` marks the row as trashed.
    #[must_use]
    pub fn from_fields(fields: &DocumentFields) -> Self {
        Self {
            status: fields.get_str("_status").map(str::to_string),
            trashed: fields.get("_deleted_at").is_some_and(|v| !v.is_null()),
        }
    }

    /// View metadata for a delete event, whose payload is intentionally empty.
    /// A soft-delete is trashed; a hard-delete is gated by the document's
    /// pre-deletion status.
    #[must_use]
    pub fn for_delete(soft_delete: bool, pre_status: Option<String>) -> Self {
        Self {
            status: pre_status,
            trashed: soft_delete,
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
    /// Per-view access-gating metadata (see [`EventViewMeta`]).
    #[serde(default)]
    pub view: EventViewMeta,
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
    pub view: EventViewMeta,
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    #[test]
    fn view_meta_from_fields_reads_status_and_trashed() {
        let mut live = DocumentFields::new();
        live.insert("_status".into(), json!("draft"));
        let m = EventViewMeta::from_fields(&live);
        assert_eq!(m.status.as_deref(), Some("draft"));
        assert!(!m.trashed, "null/absent _deleted_at is not trashed");

        let mut trashed = DocumentFields::new();
        trashed.insert("_status".into(), json!("published"));
        trashed.insert("_deleted_at".into(), json!("2026-01-01T00:00:00Z"));
        let m = EventViewMeta::from_fields(&trashed);
        assert_eq!(m.status.as_deref(), Some("published"));
        assert!(m.trashed, "non-null _deleted_at marks the row trashed");

        // Explicit null _deleted_at (e.g. an undelete) is live, not trashed.
        let mut undeleted = DocumentFields::new();
        undeleted.insert("_deleted_at".into(), Value::Null);
        assert!(!EventViewMeta::from_fields(&undeleted).trashed);

        // Status-less collection → no status axis.
        assert_eq!(
            EventViewMeta::from_fields(&DocumentFields::new()).status,
            None
        );
    }

    #[test]
    fn view_meta_for_delete_sets_trash_from_soft_delete() {
        let soft = EventViewMeta::for_delete(true, Some("published".into()));
        assert!(soft.trashed);
        assert_eq!(soft.status.as_deref(), Some("published"));

        let hard = EventViewMeta::for_delete(false, Some("draft".into()));
        assert!(!hard.trashed);
        assert_eq!(hard.status.as_deref(), Some("draft"));
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
            data: DocumentFields::new(),
            edited_by: Some(EventUser::new("u1", "u@example.com")),
            view: EventViewMeta::for_delete(false, Some("draft".into())),
        };
        let json = serde_json::to_string(&event).unwrap();
        let decoded: MutationEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.sequence, 5);
        assert_eq!(decoded.operation, EventOperation::Update);
        assert_eq!(decoded.target, EventTarget::Collection);
        assert_eq!(decoded.document_id, "abc");
        assert_eq!(decoded.edited_by.unwrap().email, "u@example.com");
        assert_eq!(decoded.view.status.as_deref(), Some("draft"));
    }
}
