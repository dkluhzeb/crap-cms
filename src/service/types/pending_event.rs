//! Mutation event queued during a transaction for post-commit publishing.

use std::{cell::RefCell, rc::Rc};

use crate::{
    core::{
        DocumentFields, Hooks, LiveSetting,
        event::{EventOperation, EventTarget, EventUser},
    },
    hooks::lifecycle::PublishEventInput,
    service::ServiceContext,
};

/// A mutation event waiting to be published after transaction commit.
pub struct PendingEvent {
    pub target: EventTarget,
    pub operation: EventOperation,
    pub collection: String,
    pub document_id: String,
    pub data: DocumentFields,
    pub edited_by: Option<EventUser>,
    pub hooks: Hooks,
    pub live: Option<LiveSetting>,
}

/// Shared queue for events accumulated during a transaction.
/// Cloning is cheap (Rc + RefCell).
pub type EventQueue = Rc<RefCell<Vec<PendingEvent>>>;

/// Flush all events from a queue, publishing each via the given context's runner + transport.
pub(crate) fn flush_queue(ctx: &ServiceContext, queue: &EventQueue) {
    let Some(runner) = ctx.runner else { return };

    let events: Vec<PendingEvent> = queue.borrow_mut().drain(..).collect();

    for pending in events {
        runner.publish_event(
            &ctx.event_transport,
            &pending.hooks,
            pending.live.as_ref(),
            PublishEventInput::builder(pending.target, pending.operation)
                .collection(pending.collection)
                .document_id(pending.document_id)
                .data(pending.data)
                .edited_by(pending.edited_by)
                .build(),
        );
    }
}
