//! Pump-side burst coalescing for the live-event streams.
//!
//! Both stream surfaces (admin SSE, gRPC Subscribe) process events strictly
//! per subscriber; the expensive part is the per-event gate + `after_read`
//! pipeline. When a subscriber falls behind a write burst, events pile up in
//! its receiver — and most of them are stale versions of the same documents.
//!
//! [`drain_and_coalesce`] empties everything already queued (non-blocking)
//! after a successful `recv` and collapses the batch **latest-wins per
//! document**: only the newest event per `(target, collection, document)`
//! survives, ordered by sequence. Two effects:
//!
//! - the gate + `after_read` pipeline runs once per *document*, not once per
//!   intermediate event, so subscribers catch up instead of lagging out;
//! - the receiver's broadcast buffer is emptied in one sweep, making
//!   `Lagged` force-drops far rarer.
//!
//! Delivery granularity under load is explicitly non-contractual (see
//! `docs/src/internals/frozen-contracts.md`): a subscriber always receives an
//! event carrying the document's **latest** state, but intermediate events
//! may collapse. A subscriber that keeps up sees every event unchanged —
//! coalescing only ever touches events that were already queued.

use std::collections::HashMap;

use super::{
    receiver::{EventReceiver, TryRecvError},
    types::{EventTarget, MutationEvent},
};

/// Upper bound on events drained per sweep — bounds pump-local memory and
/// matches the default `[live] channel_capacity`. A burst larger than this is
/// simply coalesced across multiple sweeps.
pub const MAX_DRAIN: usize = 1024;

/// Result of one drain sweep.
pub struct DrainOutcome {
    /// Coalesced events (latest-wins per document), ascending by sequence.
    pub events: Vec<MutationEvent>,
    /// The receiver reported a lag of `n` dropped events mid-drain. The
    /// caller should deliver `events` and then drop the subscriber (the
    /// same fail-safe semantic as a lagged `recv`).
    pub lagged: Option<u64>,
    /// The transport closed mid-drain; drop the subscriber after delivery.
    pub closed: bool,
}

/// Drain everything already queued on `rx` (starting from `first`, the event
/// a successful `recv` just returned), keep only the events `keep` accepts,
/// then coalesce the survivors latest-wins per document. Never blocks.
///
/// `keep` is applied to the RAW batch BEFORE coalescing — this ordering is
/// load-bearing for op-scoped gRPC subscribers: coalescing collapses to the
/// document's latest event, so filtering afterward can drop a requested event
/// (a subscriber to `create` would lose the create when a later `update` won
/// the coalesce). Admin SSE, which wants every operation, passes a pass-all
/// predicate. `max_drain` bounds how many events are pulled from `rx`, not how
/// many survive `keep`.
#[must_use]
pub fn drain_and_coalesce(
    first: MutationEvent,
    rx: &mut EventReceiver,
    max_drain: usize,
    keep: impl Fn(&MutationEvent) -> bool,
) -> DrainOutcome {
    let mut raw = Vec::new();
    if keep(&first) {
        raw.push(first);
    }

    let mut pulled = 1;
    let mut lagged = None;
    let mut closed = false;

    while pulled < max_drain {
        match rx.try_recv() {
            Ok(event) => {
                pulled += 1;
                if keep(&event) {
                    raw.push(event);
                }
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Lagged(n)) => {
                lagged = Some(n);
                break;
            }
            Err(TryRecvError::Closed) => {
                closed = true;
                break;
            }
        }
    }

    DrainOutcome {
        events: coalesce_events(raw),
        lagged,
        closed,
    }
}

/// Collapse a batch latest-wins per `(target, collection, document)`; the
/// survivors keep their own sequence/timestamp/operation and are returned in
/// ascending sequence order. A collection and a global sharing a slug stay
/// distinct (targets are namespaced, like their tables).
#[must_use]
pub fn coalesce_events(events: Vec<MutationEvent>) -> Vec<MutationEvent> {
    if events.len() <= 1 {
        return events;
    }

    let mut latest: HashMap<(bool, String, String), MutationEvent> = HashMap::new();

    for event in events {
        let key = (
            matches!(event.target, EventTarget::Global),
            event.collection.to_string(),
            event.document_id.to_string(),
        );
        // Receivers deliver in publish order, so a later entry is the newer
        // state for its document.
        latest.insert(key, event);
    }

    let mut out: Vec<MutationEvent> = latest.into_values().collect();
    out.sort_by_key(|e| e.sequence);

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        DocumentFields, DocumentId, EventViewMeta, Slug,
        event::transport::EventTransport,
        event::{EventOperation, InProcessEventBus, MutationEventInput},
    };
    use tokio::sync::broadcast;

    fn mk(sequence: u64, target: EventTarget, collection: &str, id: &str) -> MutationEvent {
        MutationEvent {
            sequence,
            timestamp: String::new(),
            target,
            operation: EventOperation::Update,
            collection: Slug::new(collection),
            document_id: DocumentId::new(id),
            data: DocumentFields::new(),
            edited_by: None,
            view: Some(EventViewMeta::default()),
        }
    }

    #[test]
    fn latest_wins_per_document() {
        let out = coalesce_events(vec![
            mk(1, EventTarget::Collection, "posts", "a"),
            mk(2, EventTarget::Collection, "posts", "a"),
            mk(3, EventTarget::Collection, "posts", "a"),
        ]);

        assert_eq!(out.len(), 1);
        assert_eq!(out[0].sequence, 3, "only the newest event survives");
    }

    #[test]
    fn distinct_documents_survive_in_sequence_order() {
        let out = coalesce_events(vec![
            mk(5, EventTarget::Collection, "posts", "b"),
            mk(3, EventTarget::Collection, "posts", "a"),
            mk(7, EventTarget::Collection, "pages", "a"),
        ]);

        let seqs: Vec<u64> = out.iter().map(|e| e.sequence).collect();
        assert_eq!(seqs, vec![3, 5, 7]);
    }

    #[test]
    fn collection_and_global_sharing_a_slug_stay_distinct() {
        let out = coalesce_events(vec![
            mk(1, EventTarget::Collection, "settings", "s"),
            mk(2, EventTarget::Global, "settings", "s"),
        ]);

        assert_eq!(out.len(), 2, "targets are namespaced like their tables");
    }

    #[test]
    fn latest_operation_wins() {
        let mut update = mk(1, EventTarget::Collection, "posts", "a");
        update.operation = EventOperation::Update;
        let mut delete = mk(2, EventTarget::Collection, "posts", "a");
        delete.operation = EventOperation::Delete;

        let out = coalesce_events(vec![update, delete]);
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].operation,
            EventOperation::Delete,
            "the delete is the document's latest state"
        );
    }

    #[test]
    fn keep_filter_applies_before_coalescing() {
        let (tx, rx) = broadcast::channel(16);
        let mut rx = EventReceiver::from_broadcast(rx);

        // Same document, created then updated within one burst.
        let mut create = mk(1, EventTarget::Collection, "posts", "a");
        create.operation = EventOperation::Create;
        let update = mk(2, EventTarget::Collection, "posts", "a"); // mk defaults to Update
        tx.send(update).unwrap();

        // A subscriber scoped to `create` must still see the create: filtering
        // before coalescing keeps it, whereas coalescing first would collapse
        // to the update and the op filter would then drop everything.
        let outcome = drain_and_coalesce(create, &mut rx, 16, |e| {
            e.operation == EventOperation::Create
        });

        assert_eq!(outcome.events.len(), 1, "the requested create must survive");
        assert_eq!(outcome.events[0].operation, EventOperation::Create);
    }

    fn publish(bus: &InProcessEventBus, collection: &str, id: &str) {
        bus.publish(MutationEventInput {
            target: EventTarget::Collection,
            operation: EventOperation::Update,
            collection: Slug::new(collection),
            document_id: DocumentId::new(id),
            data: DocumentFields::new(),
            edited_by: None,
            view: EventViewMeta::default(),
        });
    }

    #[tokio::test]
    async fn drain_collapses_a_burst() {
        let bus = InProcessEventBus::new(16);
        let mut rx = bus.subscribe();

        publish(&bus, "posts", "a");
        publish(&bus, "posts", "a");
        publish(&bus, "posts", "b");
        publish(&bus, "posts", "a");

        let first = rx.recv().await.unwrap();
        let outcome = drain_and_coalesce(first, &mut rx, MAX_DRAIN, |_| true);

        assert!(outcome.lagged.is_none());
        assert!(!outcome.closed);
        assert_eq!(outcome.events.len(), 2, "a collapses to latest, b survives");
        assert_eq!(outcome.events[0].document_id, "b");
        assert_eq!(outcome.events[1].document_id, "a");
        assert_eq!(outcome.events[1].sequence, 4, "a's survivor is the newest");
    }

    #[tokio::test]
    async fn drain_respects_the_cap() {
        let bus = InProcessEventBus::new(16);
        let mut rx = bus.subscribe();

        for i in 0..6 {
            publish(&bus, "posts", &format!("doc{i}"));
        }

        let first = rx.recv().await.unwrap();
        let outcome = drain_and_coalesce(first, &mut rx, 3, |_| true);

        assert_eq!(outcome.events.len(), 3, "cap bounds the sweep");
        // The rest stays queued for the next sweep.
        assert!(rx.try_recv().is_ok());
    }

    #[tokio::test]
    async fn drain_reports_mid_sweep_lag() {
        let bus = InProcessEventBus::new(2);
        let mut rx = bus.subscribe();

        publish(&bus, "posts", "a");
        let first = rx.recv().await.unwrap();

        // Overflow the 2-slot buffer while we hold `first`.
        for i in 0..5 {
            publish(&bus, "posts", &format!("doc{i}"));
        }

        let outcome = drain_and_coalesce(first, &mut rx, MAX_DRAIN, |_| true);

        assert!(outcome.lagged.is_some(), "mid-sweep lag must be surfaced");
        assert_eq!(
            outcome.events.len(),
            1,
            "the already-received event is delivered"
        );
    }
}
