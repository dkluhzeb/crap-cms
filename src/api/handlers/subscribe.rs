//! Subscribe handler — real-time mutation event streaming.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
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
        handlers::{ContentService, enum_mapping, proto::json_to_prost_value},
    },
    core::{
        Document, EventReceiver, FieldDefinition, FieldDenial, HookRef, LiveMode, MutationEvent,
        Registry, SharedTokenProvider,
        event::{EventOperation, EventTarget, InvalidationReceiver, RecvError},
    },
    db::{
        AccessResult, DbConnection, DbPool, EventViewGate, FilterClause,
        query::filter::memory::matches_constraints,
    },
    hooks::{AccessCheckInput, EventAfterReadInput, HookRunner},
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

/// One optional content-view axis for a slug: whether the axis exists (a status
/// axis for `draft`, soft-delete for `trash`) plus its resolved access fn
/// (`None` = default policy). An absent axis yields no view.
struct ViewAxis {
    present: bool,
    access: Option<HookRef>,
}

/// Per-slug access inputs gathered from a collection/global definition: the
/// `read` (published) ref plus the optional draft and trash view axes.
struct SlugAccess {
    read_ref: Option<HookRef>,
    draft: ViewAxis,
    trash: ViewAxis,
    fields: Vec<FieldDefinition>,
    live_mode: LiveMode,
}

/// Accumulated access state built during slug resolution.
struct AccessState {
    views: HashMap<String, EventViewGate>,
    denied_fields: HashMap<String, Vec<FieldDenial>>,
    modes: HashMap<String, LiveMode>,
}

impl AccessState {
    fn new() -> Self {
        Self {
            views: HashMap::new(),
            denied_fields: HashMap::new(),
            modes: HashMap::new(),
        }
    }
}

/// Run one view's access hook and map the outcome to visibility: `Some(filters)`
/// when allowed (empty for unconstrained), `None` when denied or the hook errors
/// (fail-closed).
fn resolve_view(
    access_ref: Option<&HookRef>,
    slug: &str,
    user_doc: Option<&Document>,
    hook_runner: &HookRunner,
    tx: &dyn DbConnection,
) -> Option<Vec<FilterClause>> {
    match hook_runner.check_access(
        &AccessCheckInput {
            access: access_ref,
            user: user_doc,
            id: None,
            data: None,
            locale: None,
            operation: "subscribe",
            collection: slug,
            ui_locale: None,
        },
        tx,
    ) {
        Ok(AccessResult::Allowed) => Some(Vec::new()),
        Ok(AccessResult::Constrained(filters)) => Some(filters),
        _ => None,
    }
}

/// Resolve every content view for a single slug, caching per-view visibility,
/// field-read denials, and live mode. A slug with no visible view is skipped.
fn resolve_single_slug(
    slug: &str,
    slug_access: &SlugAccess,
    user_doc: Option<&Document>,
    hook_runner: &HookRunner,
    tx: &dyn DbConnection,
    state: &mut AccessState,
) {
    // The draft/trash views only exist when their axis is present; an absent
    // axis (`then` short-circuits) yields no view.
    let views = EventViewGate {
        published: resolve_view(
            slug_access.read_ref.as_ref(),
            slug,
            user_doc,
            hook_runner,
            tx,
        ),
        draft: slug_access
            .draft
            .present
            .then(|| {
                resolve_view(
                    slug_access.draft.access.as_ref(),
                    slug,
                    user_doc,
                    hook_runner,
                    tx,
                )
            })
            .flatten(),
        trash: slug_access
            .trash
            .present
            .then(|| {
                resolve_view(
                    slug_access.trash.access.as_ref(),
                    slug,
                    user_doc,
                    hook_runner,
                    tx,
                )
            })
            .flatten(),
    };

    if !views.any_visible() {
        return;
    }

    let denied = hook_runner.check_field_read_access(&slug_access.fields, user_doc, None, tx);

    if !denied.is_empty() {
        state.denied_fields.insert(slug.to_string(), denied);
    }

    state.modes.insert(slug.to_string(), slug_access.live_mode);
    state.views.insert(slug.to_string(), views);
}

/// Subscriber context captured at connection time for per-event processing.
struct SubscriberCtx {
    access: SubscribeAccess,
    requested_ops: HashSet<String>,
    hook_runner: HookRunner,
    registry: Arc<Registry>,
}

/// Process a single event for a subscriber: access checks, mode-based data processing,
/// and proto conversion. Returns None if the event should be skipped.
fn process_event(event: &MutationEvent, ctx: &SubscriberCtx) -> Option<content::MutationEvent> {
    let slug_str: &str = event.collection.as_ref();

    let views = match event.target {
        EventTarget::Collection => ctx.access.collection_views.get(slug_str),
        EventTarget::Global => ctx.access.global_views.get(slug_str),
    }?;

    let op_str = match event.operation {
        EventOperation::Create => "create",
        EventOperation::Update => "update",
        EventOperation::Delete => "delete",
    };

    if !ctx.requested_ops.contains(op_str) {
        return None;
    }

    // Fail closed: an event without view metadata (e.g. from a pre-view node
    // during a rolling upgrade) cannot be safely gated, so drop it rather than
    // default to the published view.
    let view = event.view.as_ref()?;

    // Gate by the content view this event belongs to (trash/draft/published).
    // `None` means the subscriber cannot see that view, so the event is dropped
    // — closing the draft/trash leak. The event's `view` metadata is carried
    // independent of `live_mode`, so this holds even when `data` is empty
    // (metadata-only collections, all deletes).
    let constraints = views.constraints_for(view)?;

    // Row-level constraints match against the event payload. Empty `data`
    // (metadata mode / deletes) cannot satisfy a non-empty constraint, so a
    // constrained subscriber is fail-closed for those — unchanged behavior.
    if !constraints.is_empty() && !matches_constraints(&event.data, constraints) {
        return None;
    }

    let mode = ctx.access.modes.get(slug_str).copied().unwrap_or_default();

    let fields: BTreeMap<String, prost_types::Value> = if mode == LiveMode::Full {
        let (hooks, field_defs) = match event.target {
            EventTarget::Collection => ctx
                .registry
                .get_collection(slug_str)
                .map(|d| (d.hooks.clone(), d.fields.clone())),

            EventTarget::Global => ctx
                .registry
                .get_global(slug_str)
                .map(|d| (d.hooks.clone(), d.fields.clone())),
        }
        .unwrap_or_default();

        let processed = ctx
            .hook_runner
            .apply_after_read_for_event(&EventAfterReadInput {
                collection: slug_str,
                hooks: &hooks,
                fields: &field_defs,
                document_id: event.document_id.as_ref(),
                data: &event.data,
                user: ctx.access.user_doc.as_ref(),
                operation: op_str,
                timestamp: event.timestamp.as_str(),
            });

        let mut visible: serde_json::Map<String, serde_json::Value> = processed
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        if let Some(denied) = ctx.access.denied_fields.get(slug_str) {
            for denial in denied {
                denial.strip_from(&mut visible);
            }
        }

        visible
            .iter()
            .map(|(k, v)| (k.clone(), json_to_prost_value(v)))
            .collect()
    } else {
        BTreeMap::new()
    };

    Some(content::MutationEvent {
        sequence: event.sequence,
        timestamp: event.timestamp.clone(),
        target: enum_mapping::mutation_target(&event.target).into(),
        operation: enum_mapping::mutation_operation(&event.operation).into(),
        collection: event.collection.to_string(),
        document_id: event.document_id.to_string(),
        data: Some(prost_types::Struct { fields }),
    })
}

/// Resolved subscribe access: per-view visibility (with row constraints) for
/// collections and globals, field denials, modes, and the subscriber's user.
struct SubscribeAccess {
    /// Per-collection content-view access (published/draft/trash).
    collection_views: HashMap<String, EventViewGate>,
    /// Per-global content-view access (globals only carry the published view).
    global_views: HashMap<String, EventViewGate>,
    denied_fields: HashMap<String, Vec<FieldDenial>>,
    /// Per-collection event delivery mode.
    modes: HashMap<String, LiveMode>,
    /// The subscriber's user document (for per-user `after_read` hooks).
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

/// Handle one recv from the event transport.
async fn handle_event(
    tx: &mpsc::Sender<OutboundItem>,
    ctx: &SubscriberCtx,
    recv: Result<MutationEvent, RecvError>,
    send_timeout_dur: Duration,
) -> Result<(), ()> {
    match recv {
        Ok(event) => {
            let Some(out) = process_event(&event, ctx) else {
                return Ok(());
            };

            forward(tx, Ok(out), send_timeout_dur).await
        }
        Err(RecvError::Lagged(n)) => {
            warn!(
                "Subscribe stream lagged by {} events — dropping subscriber \
                 (forces reconnect); consider increasing [live] channel_capacity",
                n
            );
            Err(())
        }
        Err(RecvError::Closed) => Err(()),
    }
}

/// Handle an invalidation signal. Sends a terminal `PermissionDenied` before
/// closing if the signal targets this subscriber.
async fn handle_invalidation(
    tx: &mpsc::Sender<OutboundItem>,
    my_user_id: Option<&str>,
    recv: Result<String, RecvError>,
    send_timeout_dur: Duration,
) -> Result<(), ()> {
    let Ok(user_id) = recv else {
        // Lag or closed — keep going.
        return Ok(());
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
                    if handle_event(&tx, &ctx, recv, send_timeout_dur).await.is_err() {
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
            .event_transport
            .as_ref()
            .ok_or_else(|| Status::unavailable("Live updates disabled"))?;

        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);

        let requested_ops: HashSet<String> = if req.operations.is_empty() {
            ["create", "update", "delete"]
                .iter()
                .map(std::string::ToString::to_string)
                .collect()
        } else {
            req.operations.into_iter().collect()
        };

        let access = self
            .resolve_subscribe_access(token, headers, req.collections, req.globals)
            .await?;

        if access.collection_views.is_empty() && access.global_views.is_empty() {
            return Err(Status::permission_denied(
                "No accessible collections or globals",
            ));
        }

        let my_user_id = access.user_doc.as_ref().map(|d| d.id.to_string());

        let event_rx = event_transport.subscribe();
        let invalidation_rx = self.invalidation_transport.subscribe();
        let send_timeout_dur = Duration::from_millis(self.subscriber_send_timeout_ms);

        let (tx, rx) = mpsc::channel(SUBSCRIBER_CHANNEL_CAPACITY);

        let subscriber = SubscriberCtx {
            access,
            requested_ops,
            hook_runner: self.hook_runner.clone(),
            registry: Arc::clone(&self.registry),
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
            pool: self.pool.clone(),
            token_provider: self.token_provider.clone(),
            registry: Arc::clone(&self.registry),
            hook_runner: self.hook_runner.clone(),
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

/// Walk every requested collection + global, run their `access.read` hook (if
/// configured) under a single transaction, and return the merged access
/// outcome (allowed slugs, denied fields, hook-supplied filter constraints,
/// per-slug live-mode).
/// Resolve per-view access for the requested collections into `state`. Each
/// collection carries all three view axes (draft only with a status axis, trash
/// only with soft-delete; see [`crate::core::collection::Access`]).
fn resolve_collection_views(
    registry: &Registry,
    slugs: &[String],
    user_doc: Option<&Document>,
    hook_runner: &HookRunner,
    tx: &dyn DbConnection,
    state: &mut AccessState,
) {
    for slug in slugs {
        let Some(def) = registry.get_collection(slug) else {
            continue;
        };

        resolve_single_slug(
            slug,
            &SlugAccess {
                read_ref: def.access.read.clone(),
                draft: ViewAxis {
                    present: def.has_drafts(),
                    access: def.access.resolve_draft().cloned(),
                },
                trash: ViewAxis {
                    present: def.soft_delete,
                    access: def.access.resolve_trash().cloned(),
                },
                fields: def.fields.clone(),
                live_mode: def.live_mode,
            },
            user_doc,
            hook_runner,
            tx,
            state,
        );
    }
}

/// Resolve per-view access for the requested globals into `state`. Globals are a
/// single published row — only the `read` view exists.
fn resolve_global_views(
    registry: &Registry,
    slugs: &[String],
    user_doc: Option<&Document>,
    hook_runner: &HookRunner,
    tx: &dyn DbConnection,
    state: &mut AccessState,
) {
    for slug in slugs {
        let Some(def) = registry.get_global(slug) else {
            continue;
        };

        resolve_single_slug(
            slug,
            &SlugAccess {
                read_ref: def.access.read.clone(),
                draft: ViewAxis {
                    present: false,
                    access: None,
                },
                trash: ViewAxis {
                    present: false,
                    access: None,
                },
                fields: def.fields.clone(),
                live_mode: def.live_mode,
            },
            user_doc,
            hook_runner,
            tx,
            state,
        );
    }
}

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

    let mut col_state = AccessState::new();
    let mut global_state = AccessState::new();

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

    resolve_collection_views(
        &input.registry,
        &target_collections,
        user_doc,
        &input.hook_runner,
        &tx,
        &mut col_state,
    );

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

    resolve_global_views(
        &input.registry,
        &target_globals,
        user_doc,
        &input.hook_runner,
        &tx,
        &mut global_state,
    );

    if let Err(e) = tx.commit() {
        warn!("tx commit failed: {e}");
    }

    // Merge denied_fields and modes (keyed by slug; globals share the same maps).
    // Per-view access stays split by target to avoid collection/global slug
    // collisions during per-event lookup.
    let mut denied_fields = col_state.denied_fields;
    denied_fields.extend(global_state.denied_fields);
    let mut modes = col_state.modes;
    modes.extend(global_state.modes);

    Ok(SubscribeAccess {
        collection_views: col_state.views,
        global_views: global_state.views,
        denied_fields,
        modes,
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
}
