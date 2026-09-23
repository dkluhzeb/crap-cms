//! Mutation event payload types.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{
    core::{Document, DocumentFields, DocumentId, FieldDefinition, Slug},
    db::{
        FilterClause,
        query::filter::memory::{constraint_row, matches_constraints_typed},
    },
};

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
    /// A soft-deleted document was restored from the trash.
    Undelete,
    /// A published document or global was reverted to draft.
    Unpublish,
    /// A version snapshot was restored over the live document/global.
    Restore,
}

impl EventOperation {
    /// Canonical lowercase wire/Lua spelling — the single mapping shared by
    /// the SSE payload, subscriber op filters, and the Lua hook contexts.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
            Self::Undelete => "undelete",
            Self::Unpublish => "unpublish",
            Self::Restore => "restore",
        }
    }
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
    /// Derive view metadata from a stored row's fields — the row a write
    /// stored, or the row a delete removed as it last stood (a soft-deleted
    /// row as trashed). `_status` names the status view; a non-null
    /// `_deleted_at` marks the row as trashed, which gates the event by the
    /// trash view.
    #[must_use]
    pub fn from_fields(fields: &DocumentFields) -> Self {
        Self {
            status: fields.get_str("_status").map(str::to_string),
            trashed: fields.get("_deleted_at").is_some_and(|v| !v.is_null()),
        }
    }
}

/// The stored row a mutation event concerns, carried with the event only so a
/// subscriber's row constraints can be judged against it — never delivered.
///
/// A `Metadata`-mode event and every delete carry no document `data`, and a
/// `Full`-mode payload may be reshaped by `before_broadcast`; neither says what
/// the row holds. The snapshot does: the document's stored fields, including
/// hidden and read-denied ones (a row constraint filters on stored columns, as
/// the SQL read path does), plus `id` and the timestamps — for a delete, as
/// read just before the row was removed (a soft delete: as trashed).
///
/// Opaque by construction: nothing outside this type can read what it holds.
/// The only operation on it is [`matches`](Self::matches), so no delivery
/// encoder can copy it into a subscriber's payload, and `Debug` redacts it.
/// It is serialized only for the multi-node transport (Redis), which is
/// server-to-server.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventGateSnapshot(DocumentFields);

impl EventGateSnapshot {
    /// Snapshot a document as stored: its fields plus the `id` and timestamp
    /// columns a row constraint can name, which a [`Document`] keeps outside
    /// its field map — the same row every other in-memory judge of a document
    /// builds (see [`constraint_row`]).
    #[must_use]
    pub fn of(doc: &Document) -> Self {
        Self(constraint_row(doc))
    }

    /// Whether the snapshotted row satisfies `constraints` — the in-memory
    /// counterpart of the SQL `WHERE` a read applies, coerced by the owning
    /// definition's `fields`.
    #[must_use]
    pub fn matches(&self, constraints: &[FilterClause], fields: &[FieldDefinition]) -> bool {
        matches_constraints_typed(&self.0, constraints, fields)
    }
}

impl fmt::Debug for EventGateSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EventGateSnapshot(<{} fields redacted>)", self.0.len())
    }
}

/// A mutation event broadcast to all subscribers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationEvent {
    /// A sequence number, monotonic per [`publisher`](Self::publisher).
    pub sequence: u64,
    /// Identifies the process that published the event. Every node on a
    /// shared transport counts its own sequence from 1, so ordering and gap
    /// detection key on the `(publisher, sequence)` pair. Empty on an event
    /// from a node that predates the field.
    #[serde(default)]
    pub publisher: String,
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
    /// Per-view access-gating metadata (see [`EventViewMeta`]). `None` only on an
    /// event that arrived without it — e.g. emitted by a pre-view node during a
    /// rolling upgrade across a shared transport. Such an event cannot be safely
    /// view-gated, so consumers **drop** it (fail-closed) rather than guess a
    /// view. Current producers always set `Some`, and `Some(meta)` serializes
    /// identically to the old non-optional field, so the wire format is
    /// unchanged for new events.
    #[serde(default)]
    pub view: Option<EventViewMeta>,
    /// The stored row, for judging subscribers' row constraints only (see
    /// [`EventGateSnapshot`]). `None` when the publishing operation emitted no
    /// snapshot — an event from a node that predates it, or one whose snapshot
    /// was dropped to fit the transport's size cap. A subscriber whose view
    /// carries a row constraint never receives such an event (fail-closed);
    /// unconstrained views are unaffected. Omitted from the wire when `None`,
    /// and ignored as an unknown key by a node that predates it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<EventGateSnapshot>,
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
    pub gate: Option<EventGateSnapshot>,
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::db::{Filter, FilterOp};

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
    fn mutation_event_roundtrips_through_json() {
        // Required for the Redis transport's JSON wire format.
        let event = MutationEvent {
            sequence: 5,
            publisher: "node-a".into(),
            timestamp: "2024-01-01T00:00:00Z".into(),
            target: EventTarget::Collection,
            operation: EventOperation::Update,
            collection: Slug::new("posts"),
            document_id: DocumentId::new("abc"),
            data: DocumentFields::new(),
            edited_by: Some(EventUser::new("u1", "u@example.com")),
            view: Some(EventViewMeta {
                status: Some("draft".into()),
                trashed: false,
            }),
            gate: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        let decoded: MutationEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.sequence, 5);
        assert_eq!(decoded.publisher, "node-a");
        assert_eq!(decoded.operation, EventOperation::Update);
        assert_eq!(decoded.target, EventTarget::Collection);
        assert_eq!(decoded.document_id, "abc");
        assert_eq!(decoded.edited_by.unwrap().email, "u@example.com");
        assert_eq!(
            decoded.view.as_ref().unwrap().status.as_deref(),
            Some("draft")
        );
    }

    /// An event that arrives without `view` metadata (e.g. from a pre-view node)
    /// decodes to `None` so consumers can fail-closed, and a present view still
    /// round-trips on the unchanged wire shape.
    #[test]
    fn mutation_event_missing_view_decodes_to_none() {
        let no_view = r#"{
            "sequence": 1, "timestamp": "2024-01-01T00:00:00Z",
            "target": "collection", "operation": "update",
            "collection": "posts", "document_id": "abc",
            "data": {}, "edited_by": null
        }"#;
        let decoded: MutationEvent = serde_json::from_str(no_view).unwrap();
        assert!(
            decoded.view.is_none(),
            "absent view must decode to None (fail-closed at the consumer)"
        );
    }

    fn owned_doc() -> Document {
        let mut doc = Document::new("doc-1");
        doc.fields.insert("owner".into(), json!("u1"));
        doc.fields.insert("secret".into(), json!("s3cr3t-sentinel"));
        doc.created_at = Some("2026-01-01T00:00:00Z".into());
        doc
    }

    fn owner_is(owner: &str) -> Vec<FilterClause> {
        vec![FilterClause::Single(Filter {
            field: "owner".into(),
            op: FilterOp::Equals(owner.into()),
        })]
    }

    /// The snapshot carries the columns a row constraint can name that a
    /// `Document` keeps outside its field map — `id` and the timestamps — so a
    /// constraint such as `{ id = user.id }` is judged as SQL judges it.
    #[test]
    fn gate_snapshot_carries_id_and_timestamps() {
        let snapshot = EventGateSnapshot::of(&owned_doc());

        let id_is = |id: &str| {
            vec![FilterClause::Single(Filter {
                field: "id".into(),
                op: FilterOp::Equals(id.into()),
            })]
        };
        assert!(snapshot.matches(&id_is("doc-1"), &[]));
        assert!(!snapshot.matches(&id_is("other"), &[]));

        let created = vec![FilterClause::Single(Filter {
            field: "created_at".into(),
            op: FilterOp::Exists,
        })];
        assert!(snapshot.matches(&created, &[]));

        // An absent timestamp is not invented.
        let updated = vec![FilterClause::Single(Filter {
            field: "updated_at".into(),
            op: FilterOp::Exists,
        })];
        assert!(!snapshot.matches(&updated, &[]));
    }

    #[test]
    fn gate_snapshot_matches_the_stored_row() {
        let snapshot = EventGateSnapshot::of(&owned_doc());

        assert!(snapshot.matches(&owner_is("u1"), &[]));
        assert!(!snapshot.matches(&owner_is("u2"), &[]));
    }

    /// Logging an event must not print the stored row it carries for gating.
    #[test]
    fn gate_snapshot_debug_is_redacted() {
        let mut event = sample_event();
        event.gate = Some(EventGateSnapshot::of(&owned_doc()));

        let printed = format!("{event:?}");

        assert!(!printed.contains("s3cr3t-sentinel"), "{printed}");
        assert!(printed.contains("redacted"), "{printed}");
    }

    fn sample_event() -> MutationEvent {
        MutationEvent {
            sequence: 1,
            publisher: "node-a".into(),
            timestamp: "2024-01-01T00:00:00Z".into(),
            target: EventTarget::Collection,
            operation: EventOperation::Delete,
            collection: Slug::new("posts"),
            document_id: DocumentId::new("doc-1"),
            data: DocumentFields::new(),
            edited_by: None,
            view: Some(EventViewMeta::default()),
            gate: None,
        }
    }

    /// The multi-node transport carries the snapshot: it survives the JSON
    /// round trip and still judges constraints on the receiving node.
    #[test]
    fn gate_snapshot_roundtrips_through_json() {
        let mut event = sample_event();
        event.gate = Some(EventGateSnapshot::of(&owned_doc()));

        let json = serde_json::to_string(&event).unwrap();
        let decoded: MutationEvent = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.gate, event.gate);
        let gate = decoded.gate.expect("snapshot survives the wire");
        assert!(gate.matches(&owner_is("u1"), &[]));
        assert!(!gate.matches(&owner_is("u2"), &[]));
    }

    /// No snapshot → no `gate` key on the wire; an event without one (from a
    /// node that predates it) decodes to `None`, which the gate treats as
    /// unjudgeable for a constrained view.
    #[test]
    fn absent_gate_snapshot_is_omitted_and_decodes_to_none() {
        let json = serde_json::to_string(&sample_event()).unwrap();
        assert!(!json.contains("\"gate\""), "{json}");

        let decoded: MutationEvent = serde_json::from_str(&json).unwrap();
        assert!(decoded.gate.is_none());
    }
}
