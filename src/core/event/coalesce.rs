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
//!
//! Collapsing never hides a move between content views. The survivor
//! describes where the row is now, but a subscriber last saw it where it was
//! before the burst: the survivor carries that origin as its
//! [`EventViewMeta::prior`](super::EventViewMeta::prior), so a subscriber
//! that could see the row where it was but not where it ended up is still
//! told of the removal — an unpublish followed by a draft save, or a trash
//! followed by a purge, collapses to one event that announces it. A row that
//! ends up where it started carries no move; one created within the burst
//! did not exist before it, so it carries none either.

use std::collections::{HashMap, hash_map::Entry};

use super::{
    receiver::{EventReceiver, TryRecvError},
    types::{EventOperation, EventTarget, EventViewPlacement, MutationEvent},
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

/// An event with the position it arrived at.
type Arrived = (usize, MutationEvent);

/// Where a row sat before a burst's first event.
enum Origin {
    /// Nowhere: the burst created it.
    Created,
    /// In the views this placement selects.
    At(EventViewPlacement),
    /// Unknown: the first event carries no view metadata (every gate drops
    /// such an event), so the survivor keeps the move it carries itself.
    Unknown,
}

impl Origin {
    /// Where `event`'s row sat before it: where the event says it moved
    /// from, or where it now is when it did not move.
    fn of(event: &MutationEvent) -> Self {
        if event.operation == EventOperation::Create {
            return Self::Created;
        }

        let Some(view) = event.view.as_ref() else {
            return Self::Unknown;
        };

        let before = view.prior_view().unwrap_or_else(|| view.clone());

        Self::At(before.placement())
    }
}

/// What survives of one document's burst: its newest event, and where the row
/// sat before the burst's first event.
struct DocumentSlot {
    latest: Arrived,
    origin: Origin,
}

impl DocumentSlot {
    fn new(arrived: Arrived) -> Self {
        Self {
            origin: Origin::of(&arrived.1),
            latest: arrived,
        }
    }

    /// Supersede the newest event with `arrived`; the origin stays the one
    /// the burst started from.
    fn supersede(&mut self, arrived: Arrived) {
        self.latest = arrived;
    }

    /// The survivor, its move rebased onto the burst's origin. For a lone
    /// event that is the move it already carries.
    fn into_survivor(self) -> Arrived {
        let (arrival, mut event) = self.latest;

        let prior = match self.origin {
            Origin::Unknown => return (arrival, event),
            Origin::Created => None,
            Origin::At(placement) => Some(placement),
        };

        event.view = event.view.map(|view| view.moved_from(prior));

        (arrival, event)
    }
}

/// Collapse a batch latest-wins per `(target, collection, document)`; the
/// survivors keep their own sequence/timestamp/operation — and their own
/// gating snapshot, so the subscriber gate judges the row the surviving event
/// describes, never one it replaced — and are returned in the order their
/// final state arrived. Arrival order is the only order that
/// spans publishers: on a shared transport every node counts its own
/// `sequence` from 1, so sequence numbers from different nodes don't compare.
/// A collection and a global sharing a slug stay distinct (targets are
/// namespaced, like their tables). A survivor that replaced earlier events
/// carries the move from where the row sat before them (see the module docs).
#[must_use]
pub fn coalesce_events(events: Vec<MutationEvent>) -> Vec<MutationEvent> {
    if events.len() <= 1 {
        return events;
    }

    let mut slots: HashMap<(bool, String, String), DocumentSlot> = HashMap::new();

    for (arrival, event) in events.into_iter().enumerate() {
        let key = (
            matches!(event.target, EventTarget::Global),
            event.collection.to_string(),
            event.document_id.to_string(),
        );

        // Receivers deliver in publish order, so a later entry is the newer
        // state for its document.
        match slots.entry(key) {
            Entry::Occupied(mut slot) => slot.get_mut().supersede((arrival, event)),
            Entry::Vacant(slot) => {
                slot.insert(DocumentSlot::new((arrival, event)));
            }
        }
    }

    let mut out: Vec<Arrived> = slots
        .into_values()
        .map(DocumentSlot::into_survivor)
        .collect();
    out.sort_by_key(|(arrival, _)| *arrival);

    out.into_iter().map(|(_, event)| event).collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::sync::broadcast;

    use super::*;
    use crate::{
        core::{
            Document, DocumentFields, DocumentId, EventGateSnapshot, EventViewMeta, Slug,
            event::transport::EventTransport,
            event::{InProcessEventBus, MutationEventInput},
        },
        db::{Filter, FilterClause, FilterOp},
    };

    fn mk(sequence: u64, target: EventTarget, collection: &str, id: &str) -> MutationEvent {
        MutationEvent {
            sequence,
            publisher: String::new(),
            timestamp: String::new(),
            target,
            operation: EventOperation::Update,
            collection: Slug::new(collection),
            document_id: DocumentId::new(id),
            data: DocumentFields::new(),
            edited_by: None,
            view: Some(EventViewMeta::default()),
            gate: None,
        }
    }

    /// Survivors keep arrival order: a high sequence from one node and a low one
    /// from another say nothing about which came first.
    #[test]
    fn survivors_keep_arrival_order_across_publishers() {
        let mut from_a = mk(900, EventTarget::Collection, "posts", "x");
        from_a.publisher = "node-a".into();
        let mut from_b = mk(3, EventTarget::Collection, "posts", "y");
        from_b.publisher = "node-b".into();

        let out = coalesce_events(vec![from_a, from_b]);

        let ids: Vec<String> = out.iter().map(|e| e.document_id.to_string()).collect();
        assert_eq!(ids, ["x", "y"]);
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
    fn distinct_documents_survive_in_arrival_order() {
        let out = coalesce_events(vec![
            mk(5, EventTarget::Collection, "posts", "b"),
            mk(3, EventTarget::Collection, "posts", "a"),
            mk(7, EventTarget::Collection, "pages", "a"),
        ]);

        let seqs: Vec<u64> = out.iter().map(|e| e.sequence).collect();
        assert_eq!(seqs, vec![5, 3, 7]);
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

    fn at(status: &str, trashed: bool) -> EventViewPlacement {
        EventViewPlacement {
            status: Some(status.into()),
            trashed,
        }
    }

    /// An event for `posts/a` that left the row at `now`, having moved it from
    /// `from` (`None`: it did not move).
    fn moved(
        sequence: u64,
        operation: EventOperation,
        now: EventViewPlacement,
        from: Option<EventViewPlacement>,
    ) -> MutationEvent {
        let mut event = mk(sequence, EventTarget::Collection, "posts", "a");
        event.operation = operation;
        event.view = Some(EventViewMeta::at(now).moved_from(from));

        event
    }

    fn sequences(events: &[MutationEvent]) -> Vec<u64> {
        events.iter().map(|e| e.sequence).collect()
    }

    fn prior(event: &MutationEvent) -> Option<EventViewPlacement> {
        event.view.as_ref().and_then(|v| v.prior.clone())
    }

    /// Regression: an unpublish followed, within one burst, by a draft save or
    /// a delete of the now-draft row collapsed to the later event — gated by
    /// the draft view — so a published-only subscriber never learned the
    /// document left its view. The survivor carries the move from where the
    /// burst started.
    #[test]
    fn the_survivor_carries_the_move_from_the_burst_origin() {
        let published = at("published", false);

        for later in [EventOperation::Update, EventOperation::Delete] {
            let out = coalesce_events(vec![
                moved(1, EventOperation::Update, published.clone(), None),
                moved(
                    2,
                    EventOperation::Unpublish,
                    at("draft", false),
                    Some(published.clone()),
                ),
                moved(3, later.clone(), at("draft", false), None),
                moved(4, later.clone(), at("draft", false), None),
            ]);

            assert_eq!(sequences(&out), vec![4], "{later:?}");
            assert_eq!(prior(&out[0]), Some(published.clone()), "{later:?}");
            assert!(out[0].view.as_ref().unwrap().left_published, "{later:?}");
        }
    }

    /// Trashing a draft and purging it within one burst still tells a
    /// draft-view subscriber that cannot see the trash: the purge carries the
    /// move out of the draft view.
    #[test]
    fn a_trashed_draft_purged_in_the_burst_carries_the_move_out_of_draft() {
        let out = coalesce_events(vec![
            moved(
                1,
                EventOperation::Delete,
                at("draft", true),
                Some(at("draft", false)),
            ),
            moved(2, EventOperation::Delete, at("draft", true), None),
        ]);

        assert_eq!(sequences(&out), vec![2]);
        assert_eq!(prior(&out[0]), Some(at("draft", false)));
        assert!(!out[0].view.as_ref().unwrap().left_published);
    }

    /// Once the row is back where it started there is no move to announce:
    /// the latest state alone describes the document again.
    #[test]
    fn a_row_back_where_it_started_carries_no_move() {
        let published = at("published", false);

        let out = coalesce_events(vec![
            moved(
                1,
                EventOperation::Unpublish,
                at("draft", false),
                Some(published.clone()),
            ),
            moved(2, EventOperation::Update, at("draft", false), None),
            moved(
                3,
                EventOperation::Update,
                published.clone(),
                Some(at("draft", false)),
            ),
        ]);

        assert_eq!(sequences(&out), vec![3]);
        assert_eq!(prior(&out[0]), None);
        assert!(!out[0].view.as_ref().unwrap().left_published);
    }

    /// A row created within the burst did not exist before it, so the
    /// survivor announces no move — its own view gates it.
    #[test]
    fn a_row_created_in_the_burst_carries_no_move() {
        let out = coalesce_events(vec![
            moved(1, EventOperation::Create, at("draft", false), None),
            moved(
                2,
                EventOperation::Delete,
                at("draft", true),
                Some(at("draft", false)),
            ),
        ]);

        assert_eq!(sequences(&out), vec![2]);
        assert_eq!(prior(&out[0]), None);
    }

    /// A lone event is delivered untouched, its own move included.
    #[test]
    fn a_lone_event_keeps_its_own_move() {
        let published = at("published", false);

        let out = coalesce_events(vec![moved(
            1,
            EventOperation::Unpublish,
            at("draft", false),
            Some(published.clone()),
        )]);

        assert_eq!(prior(&out[0]), Some(published));
    }

    /// An event from a node that predates `prior` carries only the legacy
    /// flag; the burst's origin read from it is the published view.
    #[test]
    fn a_legacy_removal_sets_the_burst_origin_to_published() {
        let mut legacy = mk(1, EventTarget::Collection, "posts", "a");
        legacy.operation = EventOperation::Unpublish;
        legacy.view = Some(EventViewMeta {
            left_published: true,
            ..EventViewMeta::at(at("draft", false))
        });

        let out = coalesce_events(vec![
            legacy,
            moved(2, EventOperation::Update, at("draft", false), None),
        ]);

        assert_eq!(sequences(&out), vec![2]);
        assert_eq!(prior(&out[0]), Some(EventViewPlacement::published()));
    }

    /// A gating snapshot for a document whose `owner` is `owner`.
    fn owned_by(owner: &str) -> EventGateSnapshot {
        let mut doc = Document::new("a");
        doc.fields.insert("owner".into(), json!(owner));

        EventGateSnapshot::of(&doc)
    }

    /// The survivor of a coalesce is judged by ITS OWN stored row: a document
    /// that moved out of a subscriber's constraint must not be gated by the
    /// stale row of an event it replaced.
    #[test]
    fn survivor_keeps_its_own_gate_snapshot() {
        let mut before = mk(1, EventTarget::Collection, "posts", "a");
        before.gate = Some(owned_by("u1"));
        let mut after = mk(2, EventTarget::Collection, "posts", "a");
        after.gate = Some(owned_by("u2"));

        let out = coalesce_events(vec![before, after]);

        let owner_u1 = [FilterClause::Single(Filter {
            field: "owner".into(),
            op: FilterOp::Equals("u1".into()),
        })];
        let gate = out[0].gate.as_ref().expect("survivor carries a snapshot");

        assert_eq!(out.len(), 1);
        assert!(
            !gate.matches(&owner_u1, &[]),
            "the newest row (owner u2) decides, not the replaced one"
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
            gate: None,
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
