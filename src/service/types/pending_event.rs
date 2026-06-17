//! Mutation event queued during a transaction for post-commit publishing.

use std::{cell::RefCell, rc::Rc};

use crate::{
    core::{
        DocumentFields, Hooks, LiveSetting,
        event::{EventOperation, EventTarget, EventUser, EventViewMeta},
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
    pub view: EventViewMeta,
}

/// Shared queue for events accumulated during a transaction.
/// Cloning is cheap (Rc + `RefCell`).
pub type EventQueue = Rc<RefCell<Vec<PendingEvent>>>;

/// Tear down a user's live-update streams after an auth-document write that may
/// have changed their access — subscribers resolve access once at connect, so a
/// role/group change (via update, bulk update, restore, unpublish, or undelete)
/// must force a reconnect. No-op for non-auth collections or without an
/// invalidation transport. Call POST-COMMIT (mirrors [`flush_queue`] /
/// `publish_mutation_event`).
pub(crate) fn invalidate_user_streams_if_auth(ctx: &ServiceContext, id: &str) {
    if ctx
        .collection_def()
        .is_ok_and(crate::core::CollectionDefinition::is_auth_collection)
    {
        ctx.publish_user_invalidation(id);
    }
}

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
                .view(pending.view)
                .build(),
        );
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::{sync::Arc, time::Duration};

    use rusqlite::Connection;

    use super::*;
    use crate::core::{
        CollectionDefinition, SharedInvalidationTransport, collection::Auth,
        event::InProcessInvalidationBus,
    };

    /// Regression: an auth-collection write invalidates the user's live streams
    /// (drives update / bulk update / restore / unpublish / undelete).
    #[tokio::test]
    async fn invalidate_streams_publishes_for_auth_collection() {
        let conn = Connection::open_in_memory().unwrap();
        let mut def = CollectionDefinition::new("users");
        def.auth = Some(Auth {
            enabled: true,
            ..Default::default()
        });

        let transport: SharedInvalidationTransport = Arc::new(InProcessInvalidationBus::new());
        let mut rx = transport.subscribe();

        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .invalidation_transport(Some(transport))
            .build();

        invalidate_user_streams_if_auth(&ctx, "u1");

        let received = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("recv timed out")
            .expect("expected invalidation signal");
        assert_eq!(received, "u1");
    }

    /// A non-auth collection write never invalidates streams.
    #[tokio::test]
    async fn invalidate_streams_silent_for_non_auth_collection() {
        let conn = Connection::open_in_memory().unwrap();
        let def = CollectionDefinition::new("posts");

        let transport: SharedInvalidationTransport = Arc::new(InProcessInvalidationBus::new());
        let mut rx = transport.subscribe();

        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .invalidation_transport(Some(transport))
            .build();

        invalidate_user_streams_if_auth(&ctx, "p1");

        let res = tokio::time::timeout(Duration::from_millis(150), rx.recv()).await;
        assert!(
            res.is_err(),
            "non-auth must not publish an invalidation signal"
        );
    }
}
