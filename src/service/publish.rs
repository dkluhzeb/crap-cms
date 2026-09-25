//! Mutation-event and user-invalidation publishing for a [`ServiceContext`].
//!
//! Every write publishes through here, from the row it stored (an
//! [`EventRow`]): the event carries that row read-shaped as its document — the
//! `live` filter and `before_broadcast` hooks see it, and in `Full` mode each
//! subscriber's payload is stripped from it by that subscriber's own read
//! access — and the row's gating snapshot (see [`EventGateSnapshot`]) for
//! judging subscribers' row constraints. Nothing is stripped for the writer:
//! what one subscriber may read never depends on what the writer could.

use crate::{
    core::{
        Document, DocumentFields, EventGateSnapshot, Hooks, LiveMode, LiveSetting,
        event::{EventOperation, EventTarget, EventUser, EventViewMeta},
    },
    service::{
        Def, DeleteEvent, EventRow, ServiceContext, helpers::shape_reported, types::PendingEvent,
    },
};

/// How subscribers gate one event: the content view it belongs to and the
/// stored row their row constraints are judged against.
struct EventGating {
    view: EventViewMeta,
    gate: Option<EventGateSnapshot>,
}

impl EventGating {
    fn new(view: EventViewMeta, gate: Option<EventGateSnapshot>) -> Self {
        Self { view, gate }
    }
}

/// The definition-level parts of an event: its hooks, `live` setting, delivery
/// mode and target.
type DefParts = (Hooks, Option<LiveSetting>, LiveMode, EventTarget);

impl ServiceContext<'_> {
    /// Whether this operation publishes its own mutation events: it emits
    /// events, has a transport to publish them on, and targets a collection or
    /// global.
    #[must_use]
    pub fn publishes_events(&self) -> bool {
        self.emit_events && self.event_transport.is_some() && !matches!(self.def, Def::None)
    }

    /// Capture `doc` — the row as stored, before it is shaped or stripped for
    /// the writer — as the row this operation's event is built from, or `None`
    /// when this operation publishes no event, so a write without subscribers
    /// pays for no copy.
    #[must_use]
    pub(crate) fn event_row(&self, doc: &Document) -> Option<EventRow> {
        self.publishes_events().then(|| EventRow::new(doc))
    }

    /// Publish (or queue) a mutation event built from `row` (from
    /// [`Self::event_row`], so `None` exactly when this operation publishes
    /// nothing): its document is the stored row read-shaped, its gating
    /// snapshot and view metadata are the row's.
    ///
    /// When an `event_queue` is set (inside a transaction), the event is
    /// queued for later flushing. Otherwise it publishes immediately.
    pub(crate) fn publish_mutation_event(
        &self,
        operation: EventOperation,
        doc_id: &str,
        row: Option<EventRow>,
    ) {
        let Some(row) = row else { return };

        let gate = row.gate_snapshot();
        let prior = row.prior();
        let mut doc = row.into_document();

        shape_reported(self, &mut doc);

        let view = EventViewMeta::from_fields(&doc.fields).moved_from(prior);

        let gating = EventGating::new(view, Some(gate));

        self.publish_event(operation, doc_id, doc.fields, gating);
    }

    /// Publish (or queue) a delete event from `event` — the view the removed
    /// row was last in and its gating snapshot, both read from the row by
    /// [`read_delete_event`](crate::service::read_delete_event) (as trashed,
    /// for a soft delete) — so a constrained subscriber learns of the delete
    /// exactly when the row was one it could see. A delete delivers no
    /// document. `None` exactly when the delete publishes nothing.
    pub(crate) fn publish_delete_event(&self, doc_id: &str, event: Option<DeleteEvent>) {
        let Some(event) = event else { return };

        let (view, gate) = event.into_parts();

        self.publish_event(
            EventOperation::Delete,
            doc_id,
            DocumentFields::new(),
            EventGating::new(view, Some(gate)),
        );
    }

    /// Shared body for [`Self::publish_mutation_event`] and
    /// [`Self::publish_delete_event`]: assemble the event, then queue or
    /// publish it.
    fn publish_event(
        &self,
        operation: EventOperation,
        doc_id: &str,
        data: DocumentFields,
        gating: EventGating,
    ) {
        if !self.publishes_events() {
            return;
        }

        let Some(pending) = self.pending_event(operation, doc_id, data, gating) else {
            return;
        };

        if let Some(ref queue) = self.event_queue {
            queue.borrow_mut().push(pending);

            return;
        }

        let Some(runner) = self.runner else { return };

        pending.publish(runner, self.event_transport.as_ref());
    }

    /// The definition-level parts of this context's event, `None` without a
    /// definition.
    fn def_parts(&self) -> Option<DefParts> {
        match &self.def {
            Def::Collection(d) => Some((
                d.hooks.clone(),
                d.live.clone(),
                d.live_mode,
                EventTarget::Collection,
            )),
            Def::Global(d) => Some((
                d.hooks.clone(),
                d.live.clone(),
                d.live_mode,
                EventTarget::Global,
            )),
            Def::None => None,
        }
    }

    /// Assemble the event for this context's definition: the document, the
    /// definition's delivery mode (which decides, after the hooks, whether the
    /// document travels), the editor, and the gating. `None` without a
    /// definition.
    fn pending_event(
        &self,
        operation: EventOperation,
        doc_id: &str,
        data: DocumentFields,
        gating: EventGating,
    ) -> Option<PendingEvent> {
        let (hooks, live, mode, target) = self.def_parts()?;

        let edited_by = self.user.map(|u| {
            let email = u.get_str("email").unwrap_or_default().to_string();
            EventUser::new(u.id.to_string(), email)
        });

        Some(PendingEvent {
            target,
            operation,
            collection: self.slug.to_string(),
            document_id: doc_id.to_string(),
            data,
            edited_by,
            hooks,
            live,
            view: gating.view,
            mode,
            gate: gating.gate,
        })
    }

    /// Publish a user-invalidation signal if an invalidation transport is
    /// configured. Fire-and-forget — no-op when no transport is attached.
    ///
    /// Called from the service layer (e.g. `lock_user`, `delete_document_in_conn`
    /// for hard-delete of auth collections) so every surface that routes
    /// through the service layer gets live-stream tear-down for free.
    pub fn publish_user_invalidation(&self, user_id: &str) {
        if let Some(transport) = &self.invalidation_transport {
            transport.publish(user_id.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc, sync::Arc, time::Duration};

    use serde_json::json;
    use tokio::time::timeout;

    use super::*;
    use crate::{
        core::{
            CollectionDefinition, EventViewPlacement, SharedEventTransport,
            SharedInvalidationTransport,
            event::{InProcessEventBus, InProcessInvalidationBus},
        },
        db::{Filter, FilterClause, FilterOp},
        service::EventQueue,
    };

    #[test]
    fn publish_user_invalidation_is_noop_without_transport() {
        let def = CollectionDefinition::new("users");
        let ctx = ServiceContext::collection("users", &def).build();

        ctx.publish_user_invalidation("user-123");
        assert!(ctx.invalidation_transport.is_none());
    }

    #[tokio::test]
    async fn publish_user_invalidation_publishes_when_transport_set() {
        let bus = Arc::new(InProcessInvalidationBus::new());
        let transport: SharedInvalidationTransport = bus.clone();
        let mut rx = transport.subscribe();

        let def = CollectionDefinition::new("users");
        let ctx = ServiceContext::collection("users", &def)
            .invalidation_transport(Some(transport))
            .build();

        ctx.publish_user_invalidation("user-123");

        let received = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("recv timed out")
            .expect("expected an invalidation signal");
        assert_eq!(received, "user-123");
    }

    #[test]
    fn builder_default_transport_is_none() {
        let def = CollectionDefinition::new("users");
        let ctx = ServiceContext::collection("users", &def).build();
        assert!(ctx.invalidation_transport.is_none());
    }

    /// SAFE-DEFAULT GUARD: `emit_events` defaults to `true` so single ops keep
    /// publishing their mutation events unless a surface explicitly opts out.
    #[test]
    fn builder_emits_events_by_default() {
        let def = CollectionDefinition::new("posts");
        assert!(
            ServiceContext::collection("posts", &def)
                .build()
                .emit_events
        );
    }

    fn transport() -> SharedEventTransport {
        Arc::new(InProcessEventBus::new(16))
    }

    fn queue() -> EventQueue {
        Rc::new(RefCell::new(Vec::new()))
    }

    /// A stored row owned by `u1`, with a field no delivery may carry.
    fn stored_row() -> Document {
        let mut doc = Document::new("doc-1");
        doc.fields.insert("owner".into(), json!("u1"));
        doc.fields.insert("secret".into(), json!("s3cr3t"));
        doc
    }

    fn owner_is(owner: &str) -> [FilterClause; 1] {
        [FilterClause::Single(Filter {
            field: "owner".into(),
            op: FilterOp::Equals(owner.into()),
        })]
    }

    /// `emit_events(true)` (the default) enqueues the mutation event.
    #[test]
    fn emit_events_true_enqueues_mutation_event() {
        let queue = queue();
        let def = CollectionDefinition::new("posts");
        let ctx = ServiceContext::collection("posts", &def)
            .event_transport(Some(transport()))
            .event_queue(queue.clone())
            .build();

        ctx.publish_mutation_event(
            EventOperation::Update,
            "doc-1",
            ctx.event_row(&stored_row()),
        );
        assert_eq!(
            queue.borrow().len(),
            1,
            "default emit_events should enqueue"
        );
    }

    /// `emit_events(false)` makes `publish_mutation_event` a no-op — nothing is
    /// enqueued even with a transport and queue attached.
    #[test]
    fn emit_events_false_suppresses_mutation_event() {
        let queue = queue();
        let def = CollectionDefinition::new("posts");
        let ctx = ServiceContext::collection("posts", &def)
            .event_transport(Some(transport()))
            .event_queue(queue.clone())
            .emit_events(false)
            .build();

        ctx.publish_mutation_event(
            EventOperation::Update,
            "doc-1",
            Some(EventRow::new(&stored_row())),
        );
        assert!(
            queue.borrow().is_empty(),
            "emit_events=false must suppress the event"
        );
    }

    /// No row is captured for an operation that publishes nothing — no
    /// transport, or events switched off — so such writes pay for no copy.
    #[test]
    fn event_row_only_when_events_are_published() {
        let def = CollectionDefinition::new("posts");
        let row = stored_row();

        let no_transport = ServiceContext::collection("posts", &def).build();
        assert!(no_transport.event_row(&row).is_none());

        let quiet = ServiceContext::collection("posts", &def)
            .event_transport(Some(transport()))
            .emit_events(false)
            .build();
        assert!(quiet.event_row(&row).is_none());

        let publishing = ServiceContext::collection("posts", &def)
            .event_transport(Some(transport()))
            .build();
        let captured = publishing.event_row(&row).expect("row");
        assert!(captured.gate_snapshot().matches(&owner_is("u1"), &[]));
    }

    /// The event queued for `def` when `row` is published as an update.
    fn published(def: &CollectionDefinition, row: &Document) -> PendingEvent {
        let queue = queue();
        let ctx = ServiceContext::collection("posts", def)
            .event_transport(Some(transport()))
            .event_queue(queue.clone())
            .build();

        ctx.publish_mutation_event(EventOperation::Update, "doc-1", ctx.event_row(row));

        queue.borrow_mut().pop().expect("event queued")
    }

    /// A `Metadata`-mode event carries the stored row for its hooks and its
    /// gating snapshot, and is marked to deliver none of it (the transport
    /// input drops the document).
    #[test]
    fn metadata_mode_event_carries_the_row_and_delivers_none_of_it() {
        let event = published(&CollectionDefinition::new("posts"), &stored_row());

        assert_eq!(event.mode, LiveMode::Metadata);
        assert_eq!(event.data.get_str("owner"), Some("u1"));

        let gate = event.gate.as_ref().expect("snapshot attached");
        assert!(gate.matches(&owner_is("u1"), &[]));
        assert!(!gate.matches(&owner_is("u2"), &[]));
    }

    /// Regression: a `Full`-mode event carried the document as stripped for
    /// the WRITER, so a subscriber allowed a field the writer was denied never
    /// received it. The event now carries the stored row — every field,
    /// stripped for no one — and each subscriber's delivery strip decides.
    #[test]
    fn full_mode_event_carries_the_row_stripped_for_no_one() {
        let mut def = CollectionDefinition::new("posts");
        def.live_mode = LiveMode::Full;

        let event = published(&def, &stored_row());

        assert_eq!(event.mode, LiveMode::Full);
        assert_eq!(event.data.get_str("secret"), Some("s3cr3t"));
        assert_eq!(event.data.get_str("owner"), Some("u1"));
    }

    /// An operation without a captured row publishes nothing.
    #[test]
    fn no_row_publishes_nothing() {
        let queue = queue();
        let def = CollectionDefinition::new("posts");
        let ctx = ServiceContext::collection("posts", &def)
            .event_transport(Some(transport()))
            .event_queue(queue.clone())
            .build();

        ctx.publish_mutation_event(EventOperation::Update, "doc-1", None);

        assert!(queue.borrow().is_empty());
    }

    /// A delete event carries the removed row for gating, next to its
    /// always-empty payload and the view the row was last in.
    #[test]
    fn delete_event_carries_the_removed_row() {
        let queue = queue();
        let def = CollectionDefinition::new("posts");
        let ctx = ServiceContext::collection("posts", &def)
            .event_transport(Some(transport()))
            .event_queue(queue.clone())
            .build();
        let trashed = EventViewMeta::at(EventViewPlacement {
            status: Some("published".into()),
            trashed: true,
        });

        ctx.publish_delete_event(
            "doc-1",
            Some(DeleteEvent::new(
                trashed,
                EventGateSnapshot::of(&stored_row()),
            )),
        );

        let queued = queue.borrow();
        let event = queued.first().expect("event queued");
        assert_eq!(event.operation, EventOperation::Delete);
        assert!(event.data.is_empty());
        assert!(event.view.trashed);
        assert!(
            event
                .gate
                .as_ref()
                .is_some_and(|g| g.matches(&owner_is("u1"), &[]))
        );
    }

    /// A delete without an event publishes nothing.
    #[test]
    fn delete_without_an_event_publishes_nothing() {
        let queue = queue();
        let def = CollectionDefinition::new("posts");
        let ctx = ServiceContext::collection("posts", &def)
            .event_transport(Some(transport()))
            .event_queue(queue.clone())
            .build();

        ctx.publish_delete_event("doc-1", None);

        assert!(queue.borrow().is_empty());
    }
}
