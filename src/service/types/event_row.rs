//! The stored row a write's live event is built from.

use crate::core::{Document, EventGateSnapshot};

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
pub(crate) struct EventRow(Document);

impl EventRow {
    /// Capture `doc` as stored.
    pub(crate) fn new(doc: &Document) -> Self {
        Self(doc.clone())
    }

    /// The gating snapshot of the row (see [`EventGateSnapshot`]).
    pub(crate) fn gate_snapshot(&self) -> EventGateSnapshot {
        EventGateSnapshot::of(&self.0)
    }

    /// The stored document, for building the event's payload.
    pub(crate) fn into_document(self) -> Document {
        self.0
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
}
