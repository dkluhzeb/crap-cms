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
    /// Every operation, in declaration order.
    pub const ALL: [Self; 6] = [
        Self::Create,
        Self::Update,
        Self::Delete,
        Self::Undelete,
        Self::Unpublish,
        Self::Restore,
    ];

    /// The operations a global's event can carry: a global is never created,
    /// deleted or trashed.
    pub const GLOBAL: [Self; 3] = [Self::Update, Self::Unpublish, Self::Restore];

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

/// Where a row sits across the content views: its `_status` and whether it is
/// in the trash. The published view holds a row that is neither trashed nor a
/// draft (a collection without a status axis is always published).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventViewPlacement {
    /// The row's `_status` ("published"/"draft"); `None` for a collection
    /// without a status axis and for globals.
    pub status: Option<String>,
    /// Whether the row is in the trash.
    pub trashed: bool,
}

impl EventViewPlacement {
    /// Where a stored row sits: `_status` names its status view; a non-null
    /// `_deleted_at` puts it in the trash.
    #[must_use]
    pub fn from_fields(fields: &DocumentFields) -> Self {
        Self {
            status: fields.get_str("_status").map(str::to_string),
            trashed: fields.get("_deleted_at").is_some_and(|v| !v.is_null()),
        }
    }

    /// The placement of a row in the published view, as a node that predates
    /// [`EventViewMeta::prior`] announces a move out of it.
    #[must_use]
    pub fn published() -> Self {
        Self {
            status: Some("published".to_string()),
            trashed: false,
        }
    }

    /// Whether this placement is in the published view: not trashed and not a
    /// draft.
    #[must_use]
    pub fn in_published_view(&self) -> bool {
        !self.trashed && !self.is_draft()
    }

    /// Whether `other` selects the same content view: both in the trash or
    /// both not, and both drafts or both not (an absent status and
    /// "published" are the same published view).
    #[must_use]
    pub fn same_view(&self, other: &Self) -> bool {
        self.trashed == other.trashed && self.is_draft() == other.is_draft()
    }

    fn is_draft(&self) -> bool {
        self.status.as_deref() == Some("draft")
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
    /// Where the row sat before this mutation, when the mutation moved it
    /// between content views: a publish or unpublish, a version restore that
    /// changes the status, a move into or out of the trash. A subscriber that
    /// could see the row where it was but cannot see it where it is now is
    /// told of the removal instead of nothing (see
    /// [`EventGate`](crate::service::EventGate)). `None` when the row stayed
    /// in its view, or did not exist before (a create). Omitted from the wire
    /// when `None`; a node that predates it ignores it and reads
    /// [`left_published`](Self::left_published) instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior: Option<EventViewPlacement>,
    /// Whether this mutation moved the row OUT of the published view — derived
    /// from [`prior`](Self::prior) by [`moved_from`](Self::moved_from) and
    /// kept on the wire for nodes that predate `prior`, which announce only
    /// that one removal. An event from such a node carries this flag without
    /// a `prior`, and is read as a move out of the published view (see
    /// [`prior_view`](Self::prior_view)). Omitted from the wire when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub left_published: bool,
}

impl EventViewMeta {
    /// Derive view metadata from a stored row's fields — the row a write
    /// stored, or the row a delete removed as it last stood (a soft-deleted
    /// row as trashed). `_status` names the status view; a non-null
    /// `_deleted_at` marks the row as trashed, which gates the event by the
    /// trash view.
    #[must_use]
    pub fn from_fields(fields: &DocumentFields) -> Self {
        Self::at(EventViewPlacement::from_fields(fields))
    }

    /// View metadata for a row at `placement` that did not move.
    #[must_use]
    pub fn at(placement: EventViewPlacement) -> Self {
        Self {
            status: placement.status,
            trashed: placement.trashed,
            ..Self::default()
        }
    }

    /// Where the row sits after this mutation.
    #[must_use]
    pub fn placement(&self) -> EventViewPlacement {
        EventViewPlacement {
            status: self.status.clone(),
            trashed: self.trashed,
        }
    }

    /// Record where the row sat before the mutation (`None`: it did not
    /// exist). The single computation of a move between views: `prior` is
    /// kept only when it selects another view than where the row is now, and
    /// [`left_published`](Self::left_published) is derived from it for nodes
    /// that predate `prior`.
    #[must_use]
    pub fn moved_from(mut self, prior: Option<EventViewPlacement>) -> Self {
        self.prior = prior.filter(|prior| !prior.same_view(&self.placement()));
        self.left_published = self
            .prior
            .as_ref()
            .is_some_and(|prior| prior.in_published_view() && !self.in_published_view());

        self
    }

    /// The view the row was in before the mutation moved it — `None` when it
    /// did not move. An event from a node that predates
    /// [`prior`](Self::prior) announces only a move out of the published view.
    #[must_use]
    pub fn prior_view(&self) -> Option<Self> {
        if let Some(prior) = &self.prior {
            return Some(Self::at(prior.clone()));
        }

        self.left_published
            .then(|| Self::at(EventViewPlacement::published()))
    }

    /// Whether the row this event concerns is in the published view: not
    /// trashed and not a draft (a collection without a status axis is always
    /// published). The same selection
    /// [`EventViewGate::constraints_for`](crate::db::EventViewGate::constraints_for)
    /// gates the event by.
    #[must_use]
    pub fn in_published_view(&self) -> bool {
        !self.trashed && self.status.as_deref() != Some("draft")
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

    /// `ALL` lists every variant exactly once, and `GLOBAL` is a subset of it.
    /// The exhaustive match stops compiling when a variant is added, so the
    /// position table — and with it `ALL` — has to be revisited.
    #[test]
    fn all_lists_every_operation_once() {
        let position = |op: &EventOperation| match op {
            EventOperation::Create => 0,
            EventOperation::Update => 1,
            EventOperation::Delete => 2,
            EventOperation::Undelete => 3,
            EventOperation::Unpublish => 4,
            EventOperation::Restore => 5,
        };

        for (i, op) in EventOperation::ALL.iter().enumerate() {
            assert_eq!(position(op), i, "{op:?} is out of place in ALL");
        }

        for op in &EventOperation::GLOBAL {
            assert!(EventOperation::ALL.contains(op), "{op:?}");
        }
    }

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
            view: Some(EventViewMeta::at(draft())),
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

    fn draft() -> EventViewPlacement {
        EventViewPlacement {
            status: Some("draft".into()),
            trashed: false,
        }
    }

    fn trashed(status: &str) -> EventViewPlacement {
        EventViewPlacement {
            status: Some(status.into()),
            trashed: true,
        }
    }

    /// The prior placement and the legacy flag travel between nodes, and stay
    /// off the wire when the row did not move (the pre-field shape).
    #[test]
    fn prior_and_left_published_roundtrip_and_default_off() {
        let mut event = sample_event();
        event.view =
            Some(EventViewMeta::at(draft()).moved_from(Some(EventViewPlacement::published())));

        let json = serde_json::to_string(&event).unwrap();
        let decoded: MutationEvent = serde_json::from_str(&json).unwrap();
        let view = decoded.view.unwrap();
        assert_eq!(view.prior, Some(EventViewPlacement::published()), "{json}");
        assert!(view.left_published, "{json}");

        let plain = serde_json::to_string(&sample_event()).unwrap();
        assert!(!plain.contains("left_published"), "{plain}");
        assert!(!plain.contains("prior"), "{plain}");

        let decoded: MutationEvent = serde_json::from_str(&plain).unwrap();
        let view = decoded.view.unwrap();
        assert!(!view.left_published);
        assert!(view.prior_view().is_none());
    }

    /// An event from a node that predates `prior` carries only the legacy
    /// flag: it is read as a move out of the published view.
    #[test]
    fn a_legacy_left_published_event_reads_as_a_move_out_of_published() {
        let legacy = r#"{"status": "draft", "trashed": false, "left_published": true}"#;
        let view: EventViewMeta = serde_json::from_str(legacy).unwrap();

        assert_eq!(
            view.prior_view().map(|v| v.placement()),
            Some(EventViewPlacement::published())
        );
    }

    /// A node that predates `prior` still decodes a current event (the unknown
    /// key is ignored) and sees the published-view removal it understands.
    #[test]
    fn a_current_event_decodes_on_the_legacy_shape() {
        #[derive(Deserialize)]
        struct LegacyView {
            status: Option<String>,
            trashed: bool,
            #[serde(default)]
            left_published: bool,
        }

        let view = EventViewMeta::at(draft()).moved_from(Some(EventViewPlacement::published()));
        let legacy: LegacyView =
            serde_json::from_str(&serde_json::to_string(&view).unwrap()).unwrap();

        assert_eq!(legacy.status.as_deref(), Some("draft"));
        assert!(!legacy.trashed);
        assert!(legacy.left_published);
    }

    /// `moved_from` keeps the prior placement only when the row moved, and
    /// flags a move out of the published view — never one within it or into it.
    #[test]
    fn moved_from_records_only_a_move_between_views() {
        let published = EventViewPlacement::published();

        let unchanged = EventViewMeta::at(draft()).moved_from(Some(draft()));
        assert!(unchanged.prior.is_none() && !unchanged.left_published);

        let created = EventViewMeta::at(draft()).moved_from(None);
        assert!(created.prior.is_none() && !created.left_published);

        let unpublished = EventViewMeta::at(draft()).moved_from(Some(published.clone()));
        assert!(unpublished.left_published);

        let draft_trashed = EventViewMeta::at(trashed("draft")).moved_from(Some(draft()));
        assert_eq!(draft_trashed.prior, Some(draft()));
        assert!(!draft_trashed.left_published, "a draft never was published");

        let published_trashed =
            EventViewMeta::at(trashed("published")).moved_from(Some(published.clone()));
        assert!(published_trashed.left_published);

        let publish = EventViewMeta::at(published).moved_from(Some(draft()));
        assert_eq!(publish.prior_view().map(|v| v.placement()), Some(draft()));
        assert!(!publish.left_published);
    }

    /// An absent status and "published" are the same published view: no
    /// move between them is recorded.
    #[test]
    fn same_view_ignores_the_spelling_of_published() {
        let unset = EventViewPlacement::default();

        assert!(unset.same_view(&EventViewPlacement::published()));
        assert!(!unset.same_view(&draft()));
        assert!(!unset.same_view(&trashed("published")));
        assert!(
            EventViewMeta::at(EventViewPlacement::published())
                .moved_from(Some(unset))
                .prior
                .is_none()
        );
    }

    /// The published view is everything neither trashed nor a draft.
    #[test]
    fn in_published_view_matches_the_view_selection() {
        let view = |status: Option<&str>, trashed: bool| {
            EventViewMeta::at(EventViewPlacement {
                status: status.map(str::to_string),
                trashed,
            })
        };

        assert!(view(Some("published"), false).in_published_view());
        assert!(view(None, false).in_published_view());
        assert!(!view(Some("draft"), false).in_published_view());
        assert!(!view(Some("published"), true).in_published_view());
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
