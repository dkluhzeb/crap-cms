//! SSE endpoint for real-time mutation events in the admin UI.

use std::{
    collections::{HashMap, HashSet},
    convert::Infallible,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use axum::{
    Extension,
    extract::State,
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
};
use serde_json::{Map, Value, json};
use tokio::{sync::mpsc, time::timeout};
use tokio_stream::{Stream, wrappers::ReceiverStream};
use tokio_util::sync::WaitForCancellationFutureOwned;
use tracing::warn;

use crate::{
    admin::AdminState,
    core::{
        AuthUser, Document, Registry, Slug,
        collection::LiveMode,
        event::{
            EventOperation, EventReceiver, EventTarget, InvalidationReceiver, MutationEvent,
            RecvError,
        },
    },
    db::{AccessResult, FilterClause, query::filter::memory},
    hooks::HookRunner,
};

/// Outbound channel capacity per subscriber. Kept small — the pumping task uses
/// `send_timeout` and drops the subscriber on backpressure, so there is no point
/// queuing large numbers of events.
const SUBSCRIBER_CHANNEL_CAPACITY: usize = 16;

/// RAII guard that decrements the SSE connection counter on drop.
struct SseConnectionGuard {
    counter: Arc<AtomicUsize>,
}

impl Drop for SseConnectionGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Stream wrapper that ends when a CancellationToken fires.
/// Holds an optional SSE connection guard that decrements the counter on drop.
struct CancellableStream {
    inner: Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>,
    shutdown: Pin<Box<WaitForCancellationFutureOwned>>,
    done: bool,
    _guard: Option<SseConnectionGuard>,
}

impl Stream for CancellableStream {
    type Item = Result<Event, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.done {
            return Poll::Ready(None);
        }

        if self.shutdown.as_mut().poll(cx).is_ready() {
            self.done = true;
            return Poll::Ready(None);
        }

        self.inner.as_mut().poll_next(cx)
    }
}

/// Atomically try to acquire an SSE connection slot.
fn try_acquire_sse_slot(counter: &AtomicUsize, max: usize) -> bool {
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

/// Resolved SSE access: allowed slugs, denied fields, row-level constraints, and modes.
struct SseAccess {
    collections: HashSet<Slug>,
    globals: HashSet<Slug>,
    denied_fields: HashMap<String, Vec<String>>,
    constraints: HashMap<String, Vec<FilterClause>>,
    modes: HashMap<String, LiveMode>,
}

/// Build the set of collection/global slugs the user has read access to,
/// and cache field-level read-denied fields per collection for stream filtering.
fn build_allowed_slugs(state: &AdminState, user_doc: Option<&Document>) -> SseAccess {
    let mut access = SseAccess {
        collections: HashSet::new(),
        globals: HashSet::new(),
        denied_fields: HashMap::new(),
        constraints: HashMap::new(),
        modes: HashMap::new(),
    };

    let Ok(mut conn) = state.pool.get() else {
        return access;
    };

    let Ok(tx) = conn.transaction() else {
        return access;
    };

    for (slug, def) in &state.registry.collections {
        match state
            .hook_runner
            .check_access(def.access.read.as_deref(), user_doc, None, None, &tx)
        {
            Ok(AccessResult::Allowed) => {
                access.collections.insert(slug.clone());
            }
            Ok(AccessResult::Constrained(filters)) => {
                access.collections.insert(slug.clone());
                access.constraints.insert(slug.to_string(), filters);
            }
            _ => continue,
        }

        let denied = state
            .hook_runner
            .check_field_read_access(&def.fields, user_doc, &tx);

        if !denied.is_empty() {
            access.denied_fields.insert(slug.to_string(), denied);
        }

        access.modes.insert(slug.to_string(), def.live_mode);
    }

    for (slug, def) in &state.registry.globals {
        match state
            .hook_runner
            .check_access(def.access.read.as_deref(), user_doc, None, None, &tx)
        {
            Ok(AccessResult::Allowed) => {
                access.globals.insert(slug.clone());
            }
            Ok(AccessResult::Constrained(filters)) => {
                access.globals.insert(slug.clone());
                access.constraints.insert(slug.to_string(), filters);
            }
            _ => continue,
        }

        let denied = state
            .hook_runner
            .check_field_read_access(&def.fields, user_doc, &tx);

        if !denied.is_empty() {
            access.denied_fields.insert(slug.to_string(), denied);
        }

        access.modes.insert(slug.to_string(), def.live_mode);
    }

    if let Err(e) = tx.commit() {
        warn!("tx commit failed: {e}");
    }

    access
}

/// Build the JSON payload for an SSE event, applying access control, after_read hooks,
/// and field stripping. Returns `None` when the subscriber should not receive this event.
///
/// Separated from [`event_to_sse`] so it can be unit-tested without depending on
/// axum's opaque `sse::Event` body representation.
fn build_event_payload(
    event: &MutationEvent,
    access: &SseAccess,
    hook_runner: &HookRunner,
    registry: &Registry,
    user_doc: Option<&Document>,
) -> Option<Value> {
    let allowed = match event.target {
        EventTarget::Collection => access.collections.contains(&event.collection),
        EventTarget::Global => access.globals.contains(&event.collection),
    };

    if !allowed {
        return None;
    }

    // Row-level constraint check: skip events the subscriber can't access
    let slug_str: &str = event.collection.as_ref();

    if let Some(filters) = access.constraints.get(slug_str)
        && !memory::matches_constraints(&event.data, filters)
    {
        return None;
    }

    let target_str = match event.target {
        EventTarget::Collection => "collection",
        EventTarget::Global => "global",
    };

    let op_str = match event.operation {
        EventOperation::Create => "create",
        EventOperation::Update => "update",
        EventOperation::Delete => "delete",
    };

    // Apply mode: full = after_read hooks + data, metadata = no data
    let mode = access.modes.get(slug_str).copied().unwrap_or_default();

    let data: Map<String, Value> = if mode == LiveMode::Full {
        let (hooks, field_defs) = match event.target {
            EventTarget::Collection => registry
                .get_collection(slug_str)
                .map(|d| (d.hooks.clone(), d.fields.clone())),
            EventTarget::Global => registry
                .get_global(slug_str)
                .map(|d| (d.hooks.clone(), d.fields.clone())),
        }
        .unwrap_or_default();

        let processed_data = hook_runner.apply_after_read_for_event(
            slug_str,
            &hooks,
            &field_defs,
            event.document_id.as_ref(),
            &event.data,
            user_doc,
        );

        let denied = access.denied_fields.get(slug_str);

        processed_data
            .iter()
            .filter(|(k, _)| denied.is_none_or(|d| !d.iter().any(|name| name == *k)))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    } else {
        Map::new() // metadata mode: no data
    };

    Some(json!({
        "sequence": event.sequence,
        "timestamp": event.timestamp,
        "target": target_str,
        "operation": op_str,
        "collection": event.collection,
        "document_id": event.document_id,
        "edited_by": event.edited_by,
        "data": data,
    }))
}

/// Convert a mutation event to an SSE Event, applying access control, after_read hooks,
/// and field stripping to match normal read operations.
fn event_to_sse(
    event: &MutationEvent,
    access: &SseAccess,
    hook_runner: &HookRunner,
    registry: &Registry,
    user_doc: Option<&Document>,
) -> Option<Event> {
    let payload = build_event_payload(event, access, hook_runner, registry, user_doc)?;

    Some(
        Event::default()
            .event("mutation")
            .id(event.sequence.to_string())
            .data(payload.to_string()),
    )
}

/// Context captured for each SSE pumping task.
struct PumpCtx {
    access: SseAccess,
    hook_runner: HookRunner,
    registry: Arc<Registry>,
    user_doc: Option<Document>,
    user_id: Option<String>,
    send_timeout: Duration,
}

/// Pump one event into the outbound channel with a timeout. Returns `Err(())`
/// if the subscriber should be dropped (timeout, channel closed).
async fn forward_event(
    tx: &mpsc::Sender<Result<Event, Infallible>>,
    event: Event,
    send_timeout_dur: Duration,
) -> Result<(), ()> {
    match timeout(send_timeout_dur, tx.send(Ok(event))).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(()), // receiver dropped — client disconnected
        Err(_) => {
            warn!("SSE subscriber send timed out — dropping slow client");
            Err(())
        }
    }
}

/// Handle one event recv result. Returns `Err(())` if the subscriber should
/// be dropped.
async fn handle_broadcast_recv(
    tx: &mpsc::Sender<Result<Event, Infallible>>,
    ctx: &PumpCtx,
    recv: Result<MutationEvent, RecvError>,
) -> Result<(), ()> {
    match recv {
        Ok(event) => {
            let Some(sse_event) = event_to_sse(
                &event,
                &ctx.access,
                &ctx.hook_runner,
                &ctx.registry,
                ctx.user_doc.as_ref(),
            ) else {
                return Ok(());
            };

            forward_event(tx, sse_event, ctx.send_timeout).await
        }
        Err(RecvError::Lagged(n)) => {
            warn!(
                "SSE subscriber lagged by {} events — dropping client (forces reconnect)",
                n
            );
            Err(())
        }
        Err(RecvError::Closed) => Err(()),
    }
}

/// Handle a user-invalidation signal. Returns `Err(())` if it matches this
/// subscriber's user.
fn handle_invalidation(ctx: &PumpCtx, recv: Result<String, RecvError>) -> Result<(), ()> {
    match recv {
        Ok(user_id) => {
            let Some(my_id) = ctx.user_id.as_deref() else {
                return Ok(());
            };

            if user_id == my_id {
                warn!("SSE subscriber invalidated — user session revoked");
                return Err(());
            }

            Ok(())
        }
        // On lag or closed we treat as "stay connected" — missing a stale
        // invalidation signal is harmless; the session still gets dropped on
        // the next one. `Closed` is unreachable in practice (bus lives as long
        // as the process).
        Err(_) => Ok(()),
    }
}

/// Spawn the per-subscriber pumping task. It forwards filtered events to `tx`
/// and exits (dropping `tx`, closing the stream) on timeout, lag, or
/// user-invalidation.
fn spawn_pump(
    mut event_rx: EventReceiver,
    mut invalidation_rx: InvalidationReceiver,
    tx: mpsc::Sender<Result<Event, Infallible>>,
    ctx: PumpCtx,
) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                recv = event_rx.recv() => {
                    if handle_broadcast_recv(&tx, &ctx, recv).await.is_err() {
                        break;
                    }
                }
                recv = invalidation_rx.recv() => {
                    if handle_invalidation(&ctx, recv).is_err() {
                        break;
                    }
                }
            }
        }
    });
}

/// SSE handler — streams mutation events to authenticated admin users.
#[cfg_attr(not(tarpaulin_include), allow(dead_code))]
#[cfg(not(tarpaulin_include))]
pub async fn sse_handler(
    State(state): State<AdminState>,
    auth_user: Option<Extension<AuthUser>>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, StatusCode> {
    let max = state.max_sse_connections;

    if !try_acquire_sse_slot(&state.sse_connections, max) {
        warn!("SSE connection limit reached ({}/{}), rejecting", max, max);
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }

    let guard = SseConnectionGuard {
        counter: state.sse_connections.clone(),
    };

    let event_transport = state.event_transport.clone();
    let shutdown = state.shutdown.clone();

    let user_doc = auth_user.as_ref().map(|ext| &ext.0.user_doc);
    let access = if event_transport.is_some() {
        build_allowed_slugs(&state, user_doc)
    } else {
        SseAccess {
            collections: HashSet::new(),
            globals: HashSet::new(),
            denied_fields: HashMap::new(),
            constraints: HashMap::new(),
            modes: HashMap::new(),
        }
    };

    let hook_runner = state.hook_runner.clone();
    let registry = state.registry.clone();
    let subscriber_user_doc = auth_user.as_ref().map(|ext| ext.0.user_doc.clone());
    let subscriber_user_id = auth_user.as_ref().map(|ext| ext.0.claims.sub.to_string());
    let send_timeout = Duration::from_millis(state.subscriber_send_timeout_ms);
    let invalidation_rx = state.invalidation_transport.subscribe();

    let stream: Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>> =
        match event_transport {
            Some(transport) => {
                let event_rx = transport.subscribe();
                let (tx, rx) = mpsc::channel(SUBSCRIBER_CHANNEL_CAPACITY);

                let ctx = PumpCtx {
                    access,
                    hook_runner,
                    registry,
                    user_doc: subscriber_user_doc,
                    user_id: subscriber_user_id,
                    send_timeout,
                };

                spawn_pump(event_rx, invalidation_rx, tx, ctx);

                Box::pin(ReceiverStream::new(rx))
            }
            None => Box::pin(tokio_stream::empty()),
        };

    let stream = CancellableStream {
        inner: stream,
        shutdown: Box::pin(shutdown.cancelled_owned()),
        done: false,
        _guard: Some(guard),
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(30))))
}

#[cfg(test)]
mod tests {
    use serde_json::{Value as JsonValue, json};

    use crate::{
        config::CrapConfig,
        core::{
            DocumentId,
            collection::{Access, CollectionDefinition},
            field::{FieldAccess, FieldDefinition, FieldType},
        },
    };

    use super::*;

    #[test]
    fn sse_slot_acquire_within_limit() {
        let counter = AtomicUsize::new(0);
        assert!(try_acquire_sse_slot(&counter, 10));
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn sse_slot_acquire_at_limit() {
        let counter = AtomicUsize::new(5);
        assert!(!try_acquire_sse_slot(&counter, 5));
        assert_eq!(counter.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn sse_slot_acquire_no_limit() {
        let counter = AtomicUsize::new(1000);
        assert!(try_acquire_sse_slot(&counter, 0));
        assert_eq!(counter.load(Ordering::Relaxed), 1001);
    }

    #[test]
    fn sse_slot_fills_to_limit() {
        let counter = AtomicUsize::new(0);
        for _ in 0..3 {
            assert!(try_acquire_sse_slot(&counter, 3));
        }
        assert!(!try_acquire_sse_slot(&counter, 3));
        assert_eq!(counter.load(Ordering::Relaxed), 3);
    }

    /// Build a posts collection with one field that has field-level read access
    /// denied for everyone. Field stripping in `event_to_sse` (Full mode) must
    /// remove this field per-subscriber before emission.
    fn make_posts_with_secret_field() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.live_mode = LiveMode::Full;
        def.access = Access::default();
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition {
                name: "secret".to_string(),
                field_type: FieldType::Text,
                access: FieldAccess {
                    read: Some("hooks.access.field_read_deny".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            },
        ];
        def
    }

    fn make_event(slug: &str, data: HashMap<String, JsonValue>) -> MutationEvent {
        MutationEvent {
            sequence: 1,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            target: EventTarget::Collection,
            operation: EventOperation::Create,
            collection: Slug::new(slug),
            document_id: DocumentId::new("doc-1"),
            data,
            edited_by: None,
        }
    }

    fn fixture_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hook_tests")
    }

    fn build_runner_and_registry() -> (HookRunner, Arc<Registry>, CollectionDefinition) {
        let config_dir = fixture_dir();
        let config = CrapConfig::test_default();

        // init_lua loads the fixture's collections + hooks into a SharedRegistry.
        let shared = crate::hooks::init_lua(&config_dir, &config).expect("init lua");

        // Replace the registered "articles" with a stripped-down posts collection
        // that has the field-level read deny we need for this test.
        {
            let mut reg = shared.write().unwrap();
            reg.register_collection(make_posts_with_secret_field());
        }

        let runner = HookRunner::builder()
            .config_dir(&config_dir)
            .registry(shared.clone())
            .config(&config)
            .build()
            .expect("build runner");

        let posts = shared
            .read()
            .unwrap()
            .get_collection("posts")
            .unwrap()
            .clone();
        let registry_snapshot = Registry::snapshot(&shared);

        (runner, registry_snapshot, posts)
    }

    #[test]
    fn sse_full_mode_strips_field_read_denied_fields() {
        let (runner, registry, _posts) = build_runner_and_registry();

        // Build SseAccess that mirrors what `build_allowed_slugs` would compute
        // for an anonymous user against this posts collection.
        let mut denied_fields: HashMap<String, Vec<String>> = HashMap::new();
        denied_fields.insert("posts".to_string(), vec!["secret".to_string()]);

        let mut modes: HashMap<String, LiveMode> = HashMap::new();
        modes.insert("posts".to_string(), LiveMode::Full);

        let mut collections = HashSet::new();
        collections.insert(Slug::new("posts"));

        let access = SseAccess {
            collections,
            globals: HashSet::new(),
            denied_fields,
            constraints: HashMap::new(),
            modes,
        };

        let mut data = HashMap::new();
        data.insert("title".to_string(), json!("Hello"));
        data.insert("secret".to_string(), json!("redacted-please"));
        let event = make_event("posts", data);

        let payload = build_event_payload(&event, &access, &runner, &registry, None)
            .expect("payload should be produced for allowed collection");

        let data_obj = payload
            .get("data")
            .and_then(|v| v.as_object())
            .expect("data field should be a JSON object");

        assert_eq!(
            data_obj.get("title"),
            Some(&json!("Hello")),
            "title must be present in Full mode"
        );
        assert!(
            !data_obj.contains_key("secret"),
            "denied field 'secret' must be stripped; got: {data_obj:?}"
        );
    }

    #[test]
    fn sse_metadata_mode_omits_data_entirely() {
        let (runner, registry, _posts) = build_runner_and_registry();

        let mut modes: HashMap<String, LiveMode> = HashMap::new();
        modes.insert("posts".to_string(), LiveMode::Metadata);

        let mut collections = HashSet::new();
        collections.insert(Slug::new("posts"));

        let access = SseAccess {
            collections,
            globals: HashSet::new(),
            denied_fields: HashMap::new(),
            constraints: HashMap::new(),
            modes,
        };

        let mut data = HashMap::new();
        data.insert("title".to_string(), json!("Hello"));
        data.insert("secret".to_string(), json!("redacted-please"));
        let event = make_event("posts", data);

        let payload = build_event_payload(&event, &access, &runner, &registry, None)
            .expect("payload yielded");

        let data_obj = payload
            .get("data")
            .and_then(|v| v.as_object())
            .expect("data object present");
        assert!(
            data_obj.is_empty(),
            "metadata mode must emit empty data object; got {data_obj:?}"
        );
    }
}
