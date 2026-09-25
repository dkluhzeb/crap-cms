//! The stored row a write's live event is built from.

use crate::core::{Document, EventGateSnapshot, EventViewPlacement};

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
}

impl EventRow {
    /// Capture `doc` as stored.
    pub(crate) fn new(doc: &Document) -> Self {
        Self {
            doc: doc.clone(),
            prior: None,
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

    /// Record the `_status` the row had before a write that moves it only
    /// along the status axis — an update, which never moves a row into or out
    /// of the trash, so it sat where it still is on that axis. `None`: nothing
    /// was read, and no move is recorded.
    #[must_use]
    pub(crate) fn status_moved_from(self, status: Option<String>) -> Self {
        let Some(status) = status else {
            return self;
        };

        let prior = EventViewPlacement {
            status: Some(status),
            ..EventViewPlacement::from_fields(&self.doc.fields)
        };

        self.moved_from(Some(prior))
    }

    /// Where the row sat before the write, when recorded.
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

    /// A status-only move keeps the row's own trash state: an update of a
    /// trashed draft that publishes it moved from the trashed draft, not from
    /// the live draft view.
    #[test]
    fn status_moved_from_keeps_the_trash_state() {
        let mut doc = Document::new("doc-1");
        doc.fields.insert("_status".into(), json!("published"));
        doc.fields
            .insert("_deleted_at".into(), json!("2026-01-01T00:00:00Z"));

        let moved = EventRow::new(&doc).status_moved_from(Some("draft".into()));
        assert_eq!(
            moved.prior(),
            Some(EventViewPlacement {
                status: Some("draft".into()),
                trashed: true,
            })
        );

        assert!(
            EventRow::new(&doc)
                .status_moved_from(None)
                .prior()
                .is_none(),
            "nothing read, nothing recorded"
        );
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
