//! The stored row a write's live event is built from.

use crate::core::{Document, EventGateSnapshot, EventViewMeta, EventViewPlacement};

/// The stored row as a write found it: where it sat across the content views,
/// and — when the write can move it out of that view with changed content — a
/// gating snapshot of what it held there. Read under the write's row lock,
/// before the write persists, in the default locale the event is judged in.
#[derive(Clone)]
pub(crate) struct RowBefore {
    placement: EventViewPlacement,
    gate: Option<EventGateSnapshot>,
}

impl RowBefore {
    /// Capture `doc`, the row as stored before the write.
    pub(crate) fn of(doc: &Document) -> Self {
        Self {
            placement: EventViewPlacement::from_fields(&doc.fields),
            gate: Some(EventGateSnapshot::of(doc)),
        }
    }

    /// Only where the row sat — for a write that leaves it in that view, or
    /// moves it with its content unchanged, so no view is judged against the
    /// content it held.
    pub(crate) fn placed(placement: EventViewPlacement) -> Self {
        Self {
            placement,
            gate: None,
        }
    }
}

/// The row a write stored, as read back — hydrated, but neither shaped nor
/// stripped for anyone. A write's live event derives both of its row-dependent
/// halves from it when it is published: the gating snapshot subscribers' row
/// constraints are judged against, and the `Full`-mode payload each subscriber
/// receives after its own field-read strip. Starting delivery from the stored
/// row rather than from the document reported to the writer is what lets a
/// subscriber read a field the writer may not (and the per-subscriber strip
/// keep one the subscriber may not read away from it).
///
/// Only the event publisher consumes it; it never leaves the service layer.
#[derive(Clone)]
pub(crate) struct EventRow {
    doc: Document,
    prior: Option<EventViewPlacement>,
    prior_gate: Option<EventGateSnapshot>,
    stored: Option<EventViewPlacement>,
}

impl EventRow {
    /// Capture `doc` as stored.
    pub(crate) fn new(doc: &Document) -> Self {
        Self {
            doc: doc.clone(),
            prior: None,
            prior_gate: None,
            stored: None,
        }
    }

    /// Record where the row sat across the content views before the write
    /// (`None`: nowhere the event can announce a move from). Subscribers that
    /// could see it there but cannot see where it is now are then told of the
    /// removal (see [`EventViewMeta::moved_from`](crate::core::EventViewMeta::moved_from)).
    #[must_use]
    pub(crate) fn moved_from(mut self, prior: Option<EventViewPlacement>) -> Self {
        self.prior = prior;

        self
    }

    /// Record the content the row had in the view it left, for a move that
    /// changed it (a version restore): the left view's row constraint is
    /// judged against it (see [`EventViewMeta::left_as`]).
    #[must_use]
    pub(crate) fn left_as(mut self, before: Option<RowBefore>) -> Self {
        self.prior_gate = before.and_then(|before| before.gate);

        self
    }

    /// Record the row as an update found it (`None`: nothing was read). A
    /// published write moves the row from where it sat, content and all — a
    /// publish of a draft leaves the draft view. A draft save (`draft`)
    /// leaves the stored row where it is: its event describes the pending
    /// draft, and records where the stored row stays (see
    /// [`EventViewMeta::stored_at`]).
    #[must_use]
    pub(crate) fn before_write(mut self, before: Option<RowBefore>, draft: bool) -> Self {
        let Some(before) = before else {
            return self;
        };

        if draft {
            self.stored = Some(before.placement);

            return self;
        }

        self.prior = Some(before.placement);
        self.prior_gate = before.gate;

        self
    }

    /// The event's view metadata: where the row is now, the move recorded,
    /// the content it left, and where the stored row stays when the event
    /// describes a pending draft.
    pub(crate) fn view(&self) -> EventViewMeta {
        EventViewMeta::from_fields(&self.doc.fields)
            .moved_from(self.prior.clone())
            .left_as(self.prior_gate.clone())
            .stored_at(self.stored.clone())
    }

    /// Where the row sat before the write, when recorded.
    #[cfg(test)]
    pub(crate) fn prior(&self) -> Option<EventViewPlacement> {
        self.prior.clone()
    }

    /// The gating snapshot of the row (see [`EventGateSnapshot`]).
    pub(crate) fn gate_snapshot(&self) -> EventGateSnapshot {
        EventGateSnapshot::of(&self.doc)
    }

    /// The stored document, for building the event's payload.
    pub(crate) fn into_document(self) -> Document {
        self.doc
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::db::{Filter, FilterClause, FilterOp};

    /// The row keeps what the writer's report loses — here a field only the
    /// stored row carries — for both the snapshot and the payload.
    #[test]
    fn the_row_feeds_the_snapshot_and_the_payload() {
        let mut doc = Document::new("doc-1");
        doc.fields.insert("notes".into(), json!("internal"));

        let row = EventRow::new(&doc);
        let notes_set = [FilterClause::Single(Filter {
            field: "notes".into(),
            op: FilterOp::Equals("internal".into()),
        })];

        assert!(row.gate_snapshot().matches(&notes_set, &[]));
        assert_eq!(
            row.into_document().fields.get("notes"),
            Some(&json!("internal"))
        );
    }

    fn row(status: &str, owner: &str) -> Document {
        let mut doc = Document::new("doc-1");
        doc.fields.insert("_status".into(), json!(status));
        doc.fields.insert("owner".into(), json!(owner));

        doc
    }

    fn owner_is(owner: &str) -> [FilterClause; 1] {
        [FilterClause::Single(Filter {
            field: "owner".into(),
            op: FilterOp::Equals(owner.into()),
        })]
    }

    /// A published write records where the row was and what it held there:
    /// a publish that also changed the content is judged, in the view it
    /// left, against the old content.
    #[test]
    fn a_published_write_records_the_row_it_moved_from() {
        let before = RowBefore::of(&row("draft", "u1"));
        let after = row("published", "u2");

        let event_row = EventRow::new(&after).before_write(Some(before), false);
        let view = event_row.view();

        assert_eq!(view.prior.and_then(|p| p.status).as_deref(), Some("draft"));
        let left = view.prior_gate.expect("the content it left");
        assert!(left.matches(&owner_is("u1"), &[]));
        assert!(view.stored.is_none());
    }

    /// Regression: a draft save's event read its pending draft's placement
    /// as the stored row's. It now records where the stored row stays, and
    /// no move.
    #[test]
    fn a_draft_save_records_where_the_stored_row_stays() {
        let before = RowBefore::of(&row("published", "u1"));
        let draft = row("draft", "u1");

        let view = EventRow::new(&draft)
            .before_write(Some(before), true)
            .view();

        assert!(view.prior.is_none(), "a draft save moves nothing");
        assert!(view.prior_gate.is_none());
        assert!(view.describes_pending_draft());
        assert_eq!(
            view.stored.and_then(|p| p.status).as_deref(),
            Some("published")
        );
    }

    /// Nothing read, nothing recorded; a write that stayed in its view
    /// records no move and no left content.
    #[test]
    fn an_unmoved_row_records_nothing() {
        let doc = row("published", "u1");

        let view = EventRow::new(&doc).before_write(None, false).view();
        assert_eq!(view, EventViewMeta::from_fields(&doc.fields));

        let before = RowBefore::of(&row("published", "u0"));
        let view = EventRow::new(&doc).before_write(Some(before), false).view();
        assert!(view.prior.is_none());
        assert!(view.prior_gate.is_none(), "no move, no left content");
    }

    /// The row carries the placement it moved from, and nothing unless one is
    /// recorded.
    #[test]
    fn moved_from_carries_the_prior_placement() {
        let row = EventRow::new(&Document::new("doc-1"));
        assert!(row.prior().is_none(), "off unless recorded");

        let prior = EventViewPlacement::published();
        assert_eq!(row.moved_from(Some(prior.clone())).prior(), Some(prior));
    }
}
