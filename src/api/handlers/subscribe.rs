//! Subscribe handler — real-time mutation event streaming.

use std::{
    collections::{HashMap, HashSet},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use tokio::{sync::mpsc, task, time::timeout};
use tokio_stream::{Stream, wrappers::ReceiverStream};
use tonic::{Request, Response, Status};
use tracing::{error, warn};

use crate::{
    api::{
        content,
        handlers::{ContentService, enum_mapping, proto::json_to_field_value},
    },
    core::{
        Document, EventReceiver, MutationEvent, Registry, SharedTokenProvider,
        event::{InvalidationReceiver, MAX_DRAIN, RecvError, drain_and_coalesce},
    },
    db::DbPool,
    hooks::HookRunner,
    service::{EventAccessInput, EventAccessMap, EventGate, event_op_str},
};

/// Outbound channel capacity per subscriber. Small — we rely on `send_timeout`
/// + drop-on-backpressure rather than queuing.
const SUBSCRIBER_CHANNEL_CAPACITY: usize = 16;

/// Atomically try to acquire a Subscribe connection slot.
///
/// Returns `true` if a slot was acquired (counter incremented), `false` if the
/// limit has been reached. When `max == 0`, no limit is enforced (always succeeds).
fn try_acquire_subscribe_slot(counter: &AtomicUsize, max: usize) -> bool {
    loop {
        let current = counter.load(Ordering::Relaxed);

        if max > 0 && current >= max {
            return false;
        }

        if counter
            .compare_exchange_weak(current, current + 1, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            return true;
        }
    }
}

/// RAII guard that decrements the Subscribe connection counter on drop.
struct SubscribeConnectionGuard {
    counter: Arc<AtomicUsize>,
}

impl Drop for SubscribeConnectionGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Stream wrapper that holds a connection guard, releasing it when the stream ends.
struct GuardedStream<S> {
    inner: Pin<Box<S>>,
    _guard: SubscribeConnectionGuard,
}

impl<S: Stream + Unpin> Stream for GuardedStream<S> {
    type Item = S::Item;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

/// Subscriber context captured at connection time for per-event processing.
struct SubscriberCtx {
    access: SubscribeAccess,
    requested_ops: HashSet<String>,
    hook_runner: HookRunner,
    registry: Arc<Registry>,
}

/// Process a single event for a subscriber: requested-op filter, then the
/// shared view gate + field strip, then proto conversion. Returns None if the
/// event should be skipped. The gate is shared with the admin SSE stream so the
/// security-critical pipeline can't drift between the two surfaces.
fn process_event(event: &MutationEvent, ctx: &SubscriberCtx) -> Option<content::MutationEvent> {
    // gRPC subscribers can scope to a subset of operations; the SSE admin stream
    // always wants all, so this filter is subscribe-specific and stays here.
    if !ctx.requested_ops.contains(event_op_str(&event.operation)) {
        return None;
    }

    let visible = EventGate {
        collection_views: &ctx.access.maps.collection_views,
        global_views: &ctx.access.maps.global_views,
        collection_modes: &ctx.access.maps.collection_modes,
        global_modes: &ctx.access.maps.global_modes,
        registry: &ctx.registry,
        hook_runner: &ctx.hook_runner,
        user_doc: ctx.access.user_doc.as_ref(),
    }
    .evaluate(event)?;

    let fields: HashMap<String, content::FieldValue> = visible
        .iter()
        .map(|(k, v)| (k.clone(), json_to_field_value(v)))
        .collect();

    Some(content::MutationEvent {
        sequence: event.sequence,
        timestamp: event.timestamp.clone(),
        target: enum_mapping::mutation_target(&event.target).into(),
        operation: enum_mapping::mutation_operation(&event.operation).into(),
        collection: event.collection.to_string(),
        document_id: event.document_id.to_string(),
        data: Some(content::DataMap { fields }),
    })
}

/// Resolved subscribe access: the shared per-view access maps plus the
/// subscriber's user document (for per-user `after_read` hooks).
struct SubscribeAccess {
    maps: EventAccessMap,
    user_doc: Option<Document>,
}

/// Message type sent into the outbound channel — either a normal event or a
/// terminal status (delivered to the client before closing).
type OutboundItem = Result<content::MutationEvent, Status>;

/// Outbound channel send with timeout — returns `Err(())` if the subscriber
/// should be dropped.
async fn forward(
    tx: &mpsc::Sender<OutboundItem>,
    item: OutboundItem,
    send_timeout_dur: Duration,
) -> Result<(), ()> {
    match timeout(send_timeout_dur, tx.send(item)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(()), // client disconnected
        Err(_) => {
            warn!("Subscribe client send timed out — dropping slow subscriber");
            Err(())
        }
    }
}

/// Handle one recv from the event transport: drain-and-coalesce everything
/// already queued (bursts collapse latest-wins per document, so the gate +
/// `after_read` pipeline runs once per document), then forward the survivors.
async fn handle_event(
    tx: &mpsc::Sender<OutboundItem>,
    ctx: &SubscriberCtx,
    recv: Result<MutationEvent, RecvError>,
    event_rx: &mut EventReceiver,
    send_timeout_dur: Duration,
) -> Result<(), ()> {
    let event = match recv {
        Ok(event) => event,
        Err(RecvError::Lagged(n)) => {
            warn_subscribe_lag(n);
            return Err(());
        }
        Err(RecvError::Closed) => return Err(()),
    };

    let outcome = drain_and_coalesce(event, event_rx, MAX_DRAIN);

    // Per-event field-strip + `after_read` Lua (a VM acquire up to 5s)
    // runs for the whole batch in ONE blocking hop, off the async pump
    // worker (ledger class L12); forwarding stays async.
    let outs = crate::admin::handlers::shared::response::on_blocking_section(|| {
        outcome
            .events
            .iter()
            .filter_map(|event| process_event(event, ctx))
            .collect::<Vec<_>>()
    });

    for out in outs {
        forward(tx, Ok(out), send_timeout_dur).await?;
    }

    // Mid-sweep lag/close: the survivors above were still delivered; now
    // apply the same fail-safe as a lagged recv — drop the subscriber.
    if let Some(n) = outcome.lagged {
        warn_subscribe_lag(n);
        return Err(());
    }
    if outcome.closed {
        return Err(());
    }

    Ok(())
}

fn warn_subscribe_lag(n: u64) {
    warn!(
        "Subscribe stream lagged by {} events — dropping subscriber \
         (forces reconnect); consider increasing [live] channel_capacity",
        n
    );
}

/// Handle an invalidation signal. Sends a terminal `PermissionDenied` before
/// closing if the signal targets this subscriber.
async fn handle_invalidation(
    tx: &mpsc::Sender<OutboundItem>,
    my_user_id: Option<&str>,
    recv: Result<String, RecvError>,
    send_timeout_dur: Duration,
) -> Result<(), ()> {
    let user_id = match recv {
        Ok(user_id) => user_id,

        // Security: the invalidation bus is a fixed-capacity broadcast. If it
        // lagged we may have missed *our own* revocation, and if it closed no
        // future revocation can ever reach us — either way we can no longer
        // guarantee this session is still valid. Fail closed: drop the
        // subscriber and force a reconnect (which re-authenticates) rather than
        // keep streaming to a possibly-revoked token.
        Err(RecvError::Lagged(n)) => {
            warn!(
                "Subscribe invalidation stream lagged by {} — dropping subscriber \
                 to force re-auth (a revocation may have been missed)",
                n
            );
            return Err(());
        }
        Err(RecvError::Closed) => return Err(()),
    };

    let Some(my_id) = my_user_id else {
        return Ok(());
    };

    if user_id != my_id {
        return Ok(());
    }

    warn!("Subscribe subscriber invalidated — user session revoked");
    let _ = forward(
        tx,
        Err(Status::permission_denied(
            "User session revoked — reconnect with a fresh token",
        )),
        send_timeout_dur,
    )
    .await;

    Err(())
}

/// Spawn the pumping task that forwards events and honours invalidation.
fn spawn_pump(
    mut event_rx: EventReceiver,
    mut invalidation_rx: InvalidationReceiver,
    tx: mpsc::Sender<OutboundItem>,
    ctx: SubscriberCtx,
    my_user_id: Option<String>,
    send_timeout_dur: Duration,
) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                recv = event_rx.recv() => {
                    if handle_event(&tx, &ctx, recv, &mut event_rx, send_timeout_dur)
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                recv = invalidation_rx.recv() => {
                    if handle_invalidation(
                        &tx,
                        my_user_id.as_deref(),
                        recv,
                        send_timeout_dur,
                    ).await.is_err() {
                        break;
                    }
                }
            }
        }
    });
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Subscribe to real-time mutation events (server streaming).
    pub(in crate::api::handlers) async fn subscribe_impl(
        &self,
        request: Request<content::SubscribeRequest>,
    ) -> Result<
        Response<Pin<Box<dyn Stream<Item = Result<content::MutationEvent, Status>> + Send>>>,
        Status,
    > {
        let max = self.max_subscribe_connections;

        if !try_acquire_subscribe_slot(&self.subscribe_connections, max) {
            warn!(
                "Subscribe connection limit reached ({}/{}), rejecting",
                max, max
            );
            return Err(Status::resource_exhausted("Too many Subscribe streams"));
        }

        let subscribe_guard = SubscribeConnectionGuard {
            counter: self.subscribe_connections.clone(),
        };

        let metadata = request.metadata().clone();
        let req = request.into_inner();

        let event_transport = self
            .infra
            .event_transport
            .as_ref()
            .ok_or_else(|| Status::unavailable("Live updates disabled"))?;

        // Streaming metadata is sent once at stream open, so the proto also
        // offers `SubscribeRequest.token` as the documented fallback.
        let token = Self::bearer_or_body_token(&metadata, &req.token);
        let headers = self.metadata_headers(&metadata);

        // Empty = ALL operations, including the lifecycle ones.
        let requested_ops: HashSet<String> = if req.operations.is_empty() {
            [
                "create",
                "update",
                "delete",
                "undelete",
                "unpublish",
                "restore",
            ]
            .iter()
            .map(std::string::ToString::to_string)
            .collect()
        } else {
            req.operations.into_iter().collect()
        };

        let access = self
            .resolve_subscribe_access(token, headers, req.collections, req.globals)
            .await?;

        if access.maps.collection_views.is_empty() && access.maps.global_views.is_empty() {
            return Err(Status::permission_denied(
                "No accessible collections or globals",
            ));
        }

        let my_user_id = access.user_doc.as_ref().map(|d| d.id.to_string());

        let event_rx = event_transport.subscribe();
        let invalidation_rx = self.infra.invalidation_transport.subscribe();
        let send_timeout_dur = Duration::from_millis(self.subscriber_send_timeout_ms);

        let (tx, rx) = mpsc::channel(SUBSCRIBER_CHANNEL_CAPACITY);

        let subscriber = SubscriberCtx {
            access,
            requested_ops,
            hook_runner: self.infra.hook_runner.clone(),
            registry: Arc::clone(&self.infra.registry),
        };

        spawn_pump(
            event_rx,
            invalidation_rx,
            tx,
            subscriber,
            my_user_id,
            send_timeout_dur,
        );

        let stream = ReceiverStream::new(rx);

        let guarded = GuardedStream {
            inner: Box::pin(stream),
            _guard: subscribe_guard,
        };

        Ok(Response::new(Box::pin(guarded)
            as Pin<
                Box<dyn Stream<Item = Result<content::MutationEvent, Status>> + Send>,
            >))
    }

    /// Resolve which collections and globals the caller has read access to,
    /// and cache field-level read-denied fields per collection for stream filtering.
    async fn resolve_subscribe_access(
        &self,
        token: Option<String>,
        headers: HashMap<String, String>,
        collections_req: Vec<String>,
        globals_req: Vec<String>,
    ) -> Result<SubscribeAccess, Status> {
        let input = ResolveSubscribeAccessBlockingInput {
            pool: self.infra.pool.clone(),
            token_provider: self.infra.token_provider.clone(),
            registry: Arc::clone(&self.infra.registry),
            hook_runner: self.infra.hook_runner.clone(),
            token,
            headers,
            collections_req,
            globals_req,
        };

        task::spawn_blocking(move || resolve_subscribe_access_blocking(input))
            .await
            .inspect_err(|e| error!("Subscribe task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))?
    }
}

/// Owned bundle for the `resolve_subscribe_access` spawn-blocking body.
struct ResolveSubscribeAccessBlockingInput {
    pool: DbPool,
    token_provider: SharedTokenProvider,
    registry: Arc<Registry>,
    hook_runner: HookRunner,
    headers: HashMap<String, String>,
    token: Option<String>,
    collections_req: Vec<String>,
    globals_req: Vec<String>,
}

/// Walk every requested collection + global, run their per-view `access` hooks
/// under a single transaction via the shared [`EventAccessMap::resolve`], and
/// return the resolved access maps plus the subscriber's user document.
fn resolve_subscribe_access_blocking(
    input: ResolveSubscribeAccessBlockingInput,
) -> Result<SubscribeAccess, Status> {
    let mut conn = input
        .pool
        .get()
        .inspect_err(|e| error!("Subscribe pool error: {}", e))
        .map_err(|_| Status::internal("Internal error"))?;

    let auth_user = ContentService::resolve_auth_user(
        input.token.as_deref(),
        &input.headers,
        &*input.token_provider,
        &input.hook_runner,
        &input.registry,
        &conn,
    )?;
    let user_doc = auth_user.as_ref().map(|u| &u.user_doc);

    let tx = conn
        .transaction()
        .inspect_err(|e| error!("Subscribe tx error: {}", e))
        .map_err(|_| Status::internal("Internal error"))?;

    // Empty request = subscribe to every collection/global the user can see.
    let target_collections: Vec<String> = if input.collections_req.is_empty() {
        input
            .registry
            .collections
            .keys()
            .map(std::string::ToString::to_string)
            .collect()
    } else {
        input.collections_req
    };

    let target_globals: Vec<String> = if input.globals_req.is_empty() {
        input
            .registry
            .globals
            .keys()
            .map(std::string::ToString::to_string)
            .collect()
    } else {
        input.globals_req
    };

    let maps = EventAccessMap::resolve(&EventAccessInput {
        registry: &input.registry,
        collection_slugs: &target_collections,
        global_slugs: &target_globals,
        user_doc,
        hook_runner: &input.hook_runner,
        conn: &tx,
    });

    if let Err(e) = tx.commit() {
        warn!("tx commit failed: {e}");
    }

    Ok(SubscribeAccess {
        maps,
        user_doc: auth_user.map(|au| au.user_doc),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribe_slot_acquire_within_limit() {
        let counter = AtomicUsize::new(0);
        assert!(try_acquire_subscribe_slot(&counter, 10));
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn subscribe_slot_acquire_at_limit() {
        let counter = AtomicUsize::new(5);
        assert!(!try_acquire_subscribe_slot(&counter, 5));
        assert_eq!(counter.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn subscribe_slot_acquire_no_limit() {
        let counter = AtomicUsize::new(1000);
        assert!(try_acquire_subscribe_slot(&counter, 0));
        assert_eq!(counter.load(Ordering::Relaxed), 1001);
    }

    #[test]
    fn subscribe_slot_fills_to_limit() {
        let counter = AtomicUsize::new(0);
        for _ in 0..3 {
            assert!(try_acquire_subscribe_slot(&counter, 3));
        }
        assert!(!try_acquire_subscribe_slot(&counter, 3));
        assert_eq!(counter.load(Ordering::Relaxed), 3);
    }

    /// A lagged invalidation bus means this subscriber may have missed its own
    /// revocation — it must fail closed (drop → force re-auth), not keep
    /// streaming to a possibly-revoked token.
    #[tokio::test]
    async fn invalidation_lag_drops_subscriber_fail_closed() {
        let (tx, _rx) = mpsc::channel::<OutboundItem>(4);
        let r = handle_invalidation(
            &tx,
            Some("u1"),
            Err(RecvError::Lagged(5)),
            Duration::from_millis(50),
        )
        .await;
        assert!(r.is_err(), "lagged invalidation must drop the subscriber");
    }

    /// A closed invalidation bus means no future revocation can ever reach us —
    /// also fail closed.
    #[tokio::test]
    async fn invalidation_closed_drops_subscriber_fail_closed() {
        let (tx, _rx) = mpsc::channel::<OutboundItem>(4);
        let r = handle_invalidation(
            &tx,
            Some("u1"),
            Err(RecvError::Closed),
            Duration::from_millis(50),
        )
        .await;
        assert!(r.is_err(), "closed invalidation must drop the subscriber");
    }

    /// A revocation targeting a different user leaves this subscriber running.
    #[tokio::test]
    async fn invalidation_for_other_user_keeps_streaming() {
        let (tx, _rx) = mpsc::channel::<OutboundItem>(4);
        let r = handle_invalidation(
            &tx,
            Some("u1"),
            Ok("someone_else".to_string()),
            Duration::from_millis(50),
        )
        .await;
        assert!(r.is_ok(), "another user's revocation must not affect us");
    }

    /// A revocation targeting this user sends a terminal `PermissionDenied` and
    /// drops the subscriber.
    #[tokio::test]
    async fn invalidation_for_self_sends_denied_and_drops() {
        let (tx, mut rx) = mpsc::channel::<OutboundItem>(4);
        let r = handle_invalidation(
            &tx,
            Some("u1"),
            Ok("u1".to_string()),
            Duration::from_millis(50),
        )
        .await;
        assert!(r.is_err(), "our own revocation must drop us");

        let item = rx.recv().await.expect("a terminal item should be sent");
        let status = item.expect_err("terminal item should be a Status error");
        assert_eq!(status.code(), tonic::Code::PermissionDenied);
    }
}
