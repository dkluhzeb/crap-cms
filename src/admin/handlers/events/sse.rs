//! SSE endpoint for real-time mutation events in the admin UI.

use std::{
    collections::HashMap,
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
        AuthUser, Document, EventReceiver, FieldDenial, HookRef, LiveMode, MutationEvent, Registry,
        event::{EventOperation, EventTarget, InvalidationReceiver, RecvError},
    },
    db::{AccessResult, DbConnection, EventViewGate, FilterClause, query::filter::memory},
    hooks::{AccessCheckInput, EventAfterReadInput, HookRunner},
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

/// Stream wrapper that ends when a `CancellationToken` fires.
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

/// Resolved SSE access: per-view visibility (with row constraints) for
/// collections and globals, field denials, and modes.
struct SseAccess {
    collection_views: HashMap<String, EventViewGate>,
    global_views: HashMap<String, EventViewGate>,
    // Field denials and modes are split by target: a collection and a global
    // may share a slug (tables are namespaced), so a single slug-keyed map would
    // let one clobber the other and leak a denied field or full payload.
    collection_denied_fields: HashMap<String, Vec<FieldDenial>>,
    global_denied_fields: HashMap<String, Vec<FieldDenial>>,
    collection_modes: HashMap<String, LiveMode>,
    global_modes: HashMap<String, LiveMode>,
}

impl SseAccess {
    fn empty() -> Self {
        Self {
            collection_views: HashMap::new(),
            global_views: HashMap::new(),
            collection_denied_fields: HashMap::new(),
            global_denied_fields: HashMap::new(),
            collection_modes: HashMap::new(),
            global_modes: HashMap::new(),
        }
    }
}

/// Run one content view's access hook and map the outcome to visibility:
/// `Some(filters)` when allowed (empty for unconstrained), `None` when denied or
/// the hook errors (fail-closed).
fn resolve_view(
    hook_runner: &HookRunner,
    access_ref: Option<&HookRef>,
    user_doc: Option<&Document>,
    conn: &dyn DbConnection,
    slug: &str,
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
        conn,
    ) {
        Ok(AccessResult::Allowed) => Some(Vec::new()),
        Ok(AccessResult::Constrained(filters)) => Some(filters),
        _ => None,
    }
}

/// Build per-view access for every collection/global the user can see, caching
/// field-level read denials and live mode per slug for stream filtering.
fn build_allowed_slugs(state: &AdminState, user_doc: Option<&Document>) -> SseAccess {
    let mut access = SseAccess::empty();

    let Ok(mut conn) = state.pool.get() else {
        return access;
    };

    let Ok(tx) = conn.transaction() else {
        return access;
    };

    let runner = &state.hook_runner;

    for (slug, def) in &state.registry.collections {
        // Draft view only exists with a status axis; trash only with soft-delete.
        let gate = EventViewGate {
            published: resolve_view(runner, def.access.read.as_ref(), user_doc, &tx, slug),
            draft: def
                .has_drafts()
                .then(|| resolve_view(runner, def.access.resolve_draft(), user_doc, &tx, slug))
                .flatten(),
            trash: def
                .soft_delete
                .then(|| resolve_view(runner, def.access.resolve_trash(), user_doc, &tx, slug))
                .flatten(),
        };

        if !gate.any_visible() {
            continue;
        }

        let denied = runner.check_field_read_access(&def.fields, user_doc, None, &tx);

        if !denied.is_empty() {
            access
                .collection_denied_fields
                .insert(slug.to_string(), denied);
        }

        access
            .collection_modes
            .insert(slug.to_string(), def.live_mode);
        access.collection_views.insert(slug.to_string(), gate);
    }

    for (slug, def) in &state.registry.globals {
        // Globals are a single published row — no status or trash axis.
        let gate = EventViewGate {
            published: resolve_view(runner, def.access.read.as_ref(), user_doc, &tx, slug),
            draft: None,
            trash: None,
        };

        if !gate.any_visible() {
            continue;
        }

        let denied = runner.check_field_read_access(&def.fields, user_doc, None, &tx);

        if !denied.is_empty() {
            access.global_denied_fields.insert(slug.to_string(), denied);
        }

        access.global_modes.insert(slug.to_string(), def.live_mode);
        access.global_views.insert(slug.to_string(), gate);
    }

    if let Err(e) = tx.commit() {
        warn!("tx commit failed: {e}");
    }

    access
}

/// Build the JSON payload for an SSE event, applying access control, `after_read` hooks,
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
    let slug_str: &str = event.collection.as_ref();

    let views = match event.target {
        EventTarget::Collection => access.collection_views.get(slug_str),
        EventTarget::Global => access.global_views.get(slug_str),
    }?;

    // Fail closed: an event without view metadata (e.g. from a pre-view node
    // during a rolling upgrade) cannot be safely gated, so drop it rather than
    // default to the published view.
    let view = event.view.as_ref()?;

    // Gate by the content view this event belongs to (trash/draft/published).
    // `None` means the subscriber cannot see that view, so the event is dropped
    // — closing the draft/trash leak. The `view` metadata is carried
    // independent of `live_mode`, so this holds for empty-`data` events too
    // (metadata-only collections, all deletes).
    let constraints = views.constraints_for(view)?;

    // Row-level constraints match against the event payload; empty `data`
    // cannot satisfy a non-empty constraint (fail-closed) — unchanged behavior.
    if !constraints.is_empty() && !memory::matches_constraints(&event.data, constraints) {
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

    // Apply mode: full = after_read hooks + data, metadata = no data. Select by
    // target — a collection and a global may share a slug.
    let mode = match event.target {
        EventTarget::Collection => access.collection_modes.get(slug_str),
        EventTarget::Global => access.global_modes.get(slug_str),
    }
    .copied()
    .unwrap_or_default();

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

        let processed_data = hook_runner.apply_after_read_for_event(&EventAfterReadInput {
            collection: slug_str,
            hooks: &hooks,
            fields: &field_defs,
            document_id: event.document_id.as_ref(),
            data: &event.data,
            user: user_doc,
            operation: op_str,
            timestamp: event.timestamp.as_str(),
        });

        let mut visible: Map<String, Value> = processed_data
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let denied = match event.target {
            EventTarget::Collection => access.collection_denied_fields.get(slug_str),
            EventTarget::Global => access.global_denied_fields.get(slug_str),
        };
        if let Some(denied) = denied {
            for denial in denied {
                denial.strip_from(&mut visible);
            }
        }

        visible
    } else {
        Map::new() // metadata mode: no data
    };

    // Editor identity is server-side only — leaking the editor's id/email to
    // every subscriber is a PII exposure. Subscribers get a boolean "was this
    // my own edit" computed here instead; editor-based logic belongs in the
    // server-side `live` filter / `before_broadcast` hooks, whose contexts
    // keep the full `edited_by`.
    let is_self = match (&event.edited_by, user_doc) {
        (Some(editor), Some(user)) => editor.id == *user.id,
        _ => false,
    };

    Some(json!({
        "sequence": event.sequence,
        "timestamp": event.timestamp,
        "target": target_str,
        "operation": op_str,
        "collection": event.collection,
        "document_id": event.document_id,
        "self": is_self,
        "data": data,
    }))
}

/// Convert a mutation event to an SSE Event, applying access control, `after_read` hooks,
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
        SseAccess::empty()
    };

    let hook_runner = state.hook_runner.clone();
    let registry = Arc::clone(&state.registry);
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
    use serde_json::json;

    use crate::{
        config::CrapConfig,
        core::{
            Access, CollectionDefinition, DocumentFields, DocumentId, FieldAccess, FieldDefinition,
            FieldType,
        },
    };

    use super::*;
    use crate::core::Slug;
    use crate::core::event::{EventUser, EventViewMeta};

    /// A `collection_views` map granting the unconstrained published view for one
    /// slug — the common fixture for these payload tests.
    fn published_views(slug: &str) -> HashMap<String, EventViewGate> {
        let mut views = HashMap::new();
        views.insert(
            slug.to_string(),
            EventViewGate {
                published: Some(Vec::new()),
                draft: None,
                trash: None,
            },
        );
        views
    }

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
            // A USER-DEFINED field that happens to be named `edited_by` — it
            // lives inside the payload's `data` and goes through the normal
            // field-access pipeline, unlike the removed top-level transport
            // key of the same name.
            FieldDefinition::builder("edited_by", FieldType::Text).build(),
            FieldDefinition {
                name: "secret".to_string(),
                field_type: FieldType::Text,
                access: FieldAccess {
                    read: Some(HookRef::new("hooks.access.field_read_deny")),
                    ..Default::default()
                },
                ..Default::default()
            },
        ];
        def
    }

    fn make_event(slug: &str, data: DocumentFields) -> MutationEvent {
        MutationEvent {
            sequence: 1,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            target: EventTarget::Collection,
            operation: EventOperation::Create,
            collection: Slug::new(slug),
            document_id: DocumentId::new("doc-1"),
            data,
            edited_by: None,
            view: Some(EventViewMeta::default()),
        }
    }

    fn fixture_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hook_tests")
    }

    fn build_runner_and_registry() -> (HookRunner, Arc<Registry>, CollectionDefinition) {
        let config_dir = fixture_dir();
        let config = CrapConfig::test_default();

        // init_lua loads the fixture's collections + hooks into a snapshot Arc.
        let mut registry = crate::hooks::init_lua(&config_dir, &config).expect("init lua");

        // Inject a stripped-down posts collection with the field-level read deny
        // this test needs. `Arc::make_mut` clones if not uniquely owned (no other
        // clones exist yet at this point) and returns &mut Registry.
        Arc::make_mut(&mut registry).register_collection(make_posts_with_secret_field());

        let runner = HookRunner::builder()
            .config_dir(&config_dir)
            .registry(Arc::clone(&registry))
            .config(&config)
            .build()
            .expect("build runner");

        let posts = registry.get_collection("posts").unwrap().clone();

        (runner, registry, posts)
    }

    #[test]
    fn sse_full_mode_strips_field_read_denied_fields() {
        let (runner, registry, _posts) = build_runner_and_registry();

        // Build SseAccess that mirrors what `build_allowed_slugs` would compute
        // for an anonymous user against this posts collection.
        let mut denied_fields: HashMap<String, Vec<FieldDenial>> = HashMap::new();
        denied_fields.insert(
            "posts".to_string(),
            vec![FieldDenial::Flat("secret".into())],
        );

        let mut modes: HashMap<String, LiveMode> = HashMap::new();
        modes.insert("posts".to_string(), LiveMode::Full);

        let access = SseAccess {
            collection_views: published_views("posts"),
            global_views: HashMap::new(),
            collection_denied_fields: denied_fields,
            global_denied_fields: HashMap::new(),
            collection_modes: modes,
            global_modes: HashMap::new(),
        };

        let mut data = DocumentFields::new();
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

        let access = SseAccess {
            collection_views: published_views("posts"),
            global_views: HashMap::new(),
            collection_denied_fields: HashMap::new(),
            global_denied_fields: HashMap::new(),
            collection_modes: modes,
            global_modes: HashMap::new(),
        };

        let mut data = DocumentFields::new();
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

    /// Security: the SSE payload must never expose the editing user's
    /// identity (id/email) to subscribers — only the server-computed `self`
    /// boolean, true exactly when the authenticated subscriber IS the editor.
    /// (It previously sent the full `edited_by` `{id, email}` object to every
    /// subscriber.)
    #[test]
    fn sse_payload_exposes_self_flag_but_never_editor_identity() {
        let (runner, registry, _posts) = build_runner_and_registry();

        let access = SseAccess {
            collection_views: published_views("posts"),
            global_views: HashMap::new(),
            collection_denied_fields: HashMap::new(),
            global_denied_fields: HashMap::new(),
            collection_modes: HashMap::new(),
            global_modes: HashMap::new(),
        };

        let mut event = make_event("posts", DocumentFields::new());
        event.edited_by = Some(EventUser::new("user-1", "editor@example.com"));

        // Anonymous subscriber → self = false, and no identity anywhere in
        // the serialized payload.
        let payload = build_event_payload(&event, &access, &runner, &registry, None)
            .expect("payload yielded");
        assert_eq!(payload.get("self"), Some(&json!(false)));
        assert!(
            payload.get("edited_by").is_none(),
            "edited_by must not be in the payload: {payload}"
        );
        let raw = payload.to_string();
        assert!(
            !raw.contains("user-1") && !raw.contains("editor@example.com"),
            "payload must not leak editor identity: {raw}"
        );

        // The subscriber IS the editor → self = true.
        let me = Document::new("user-1");
        let payload = build_event_payload(&event, &access, &runner, &registry, Some(&me))
            .expect("payload yielded");
        assert_eq!(payload.get("self"), Some(&json!(true)));

        // A different authenticated subscriber → self = false.
        let other = Document::new("user-2");
        let payload = build_event_payload(&event, &access, &runner, &registry, Some(&other))
            .expect("payload yielded");
        assert_eq!(payload.get("self"), Some(&json!(false)));
    }

    /// A USER-DEFINED field named `edited_by` is ordinary document data: it
    /// must flow through to `payload.data` (subject to field access like any
    /// other field). Only the old top-level transport key of the same name —
    /// the editor-identity leak — is gone.
    #[test]
    fn user_field_named_edited_by_still_flows_through_data() {
        let (runner, registry, _posts) = build_runner_and_registry();

        let mut modes: HashMap<String, LiveMode> = HashMap::new();
        modes.insert("posts".to_string(), LiveMode::Full);

        let access = SseAccess {
            collection_views: published_views("posts"),
            global_views: HashMap::new(),
            collection_denied_fields: HashMap::new(),
            global_denied_fields: HashMap::new(),
            collection_modes: modes,
            global_modes: HashMap::new(),
        };

        let mut data = DocumentFields::new();
        data.insert("edited_by".to_string(), json!("a plain document value"));
        let mut event = make_event("posts", data);
        event.edited_by = Some(EventUser::new("user-1", "editor@example.com"));

        let payload = build_event_payload(&event, &access, &runner, &registry, None)
            .expect("payload yielded");

        // The user field is present inside `data`...
        assert_eq!(
            payload.get("data").and_then(|d| d.get("edited_by")),
            Some(&json!("a plain document value")),
            "user-defined edited_by field must pass through: {payload}"
        );
        // ...while the editor's identity still appears nowhere.
        let raw = payload.to_string();
        assert!(
            !raw.contains("user-1") && !raw.contains("editor@example.com"),
            "payload must not leak editor identity: {raw}"
        );
    }

    /// Regression: a collection and a global may share a slug (tables are
    /// namespaced, no cross-uniqueness check). Their delivery `modes` and field
    /// denials must be looked up by event target, not merged into one
    /// slug-keyed map — otherwise one clobbers the other and leaks the full
    /// payload where metadata was configured (or vice-versa). Here the
    /// collection `posts` is Full and the global `posts` is Metadata; each event
    /// must honor its own target's mode.
    #[test]
    fn sse_collection_and_global_sharing_slug_do_not_collide() {
        let (runner, registry, _posts) = build_runner_and_registry();

        let mut collection_modes = HashMap::new();
        collection_modes.insert("posts".to_string(), LiveMode::Full);
        let mut global_modes = HashMap::new();
        global_modes.insert("posts".to_string(), LiveMode::Metadata);

        let access = SseAccess {
            collection_views: published_views("posts"),
            global_views: published_views("posts"),
            collection_denied_fields: HashMap::new(),
            global_denied_fields: HashMap::new(),
            collection_modes,
            global_modes,
        };

        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("Hello"));

        // Collection event → Full → data present.
        let col_event = make_event("posts", data.clone());
        let col_payload = build_event_payload(&col_event, &access, &runner, &registry, None)
            .expect("collection payload");
        assert_eq!(
            col_payload.get("data").and_then(|d| d.get("title")),
            Some(&json!("Hello")),
            "collection 'posts' is Full mode — data must be present: {col_payload}"
        );

        // Global event (same slug) → Metadata → empty data. Pre-fix the merged
        // map would have made the collection Metadata too (global clobbered it).
        let mut global_event = make_event("posts", data);
        global_event.target = EventTarget::Global;
        let global_payload = build_event_payload(&global_event, &access, &runner, &registry, None)
            .expect("global payload");
        assert!(
            global_payload
                .get("data")
                .and_then(|d| d.as_object())
                .expect("data object")
                .is_empty(),
            "global 'posts' is Metadata mode — data must be empty: {global_payload}"
        );
    }

    /// Build an `SseAccess` exposing exactly `gate` for the "posts" collection.
    fn access_with_gate(gate: EventViewGate) -> SseAccess {
        let mut collection_views = HashMap::new();
        collection_views.insert("posts".to_string(), gate);

        SseAccess {
            collection_views,
            global_views: HashMap::new(),
            collection_denied_fields: HashMap::new(),
            global_denied_fields: HashMap::new(),
            collection_modes: HashMap::new(),
            global_modes: HashMap::new(),
        }
    }

    fn event_with_view(status: Option<&str>, trashed: bool) -> MutationEvent {
        let mut event = make_event("posts", DocumentFields::new());
        event.view = Some(EventViewMeta {
            status: status.map(str::to_string),
            trashed,
        });
        event
    }

    /// Regression: a `read`-only subscriber must NOT receive draft mutation
    /// events. Before P4, drafts streamed regardless of the subscriber's view.
    #[test]
    fn draft_event_dropped_when_only_published_visible() {
        let (runner, registry, _posts) = build_runner_and_registry();

        let access = access_with_gate(EventViewGate {
            published: Some(Vec::new()),
            draft: None,
            trash: None,
        });
        let event = event_with_view(Some("draft"), false);

        assert!(
            build_event_payload(&event, &access, &runner, &registry, None).is_none(),
            "draft event must be withheld from a published-only subscriber"
        );
    }

    /// Fail-closed: an event whose view metadata is absent (e.g. emitted by a
    /// pre-view node during a rolling upgrade) is dropped — never defaulted to
    /// the published view — even for a fully-visible subscriber.
    #[test]
    fn view_less_event_is_dropped() {
        let (runner, registry, _posts) = build_runner_and_registry();

        let access = access_with_gate(EventViewGate {
            published: Some(Vec::new()),
            draft: Some(Vec::new()),
            trash: Some(Vec::new()),
        });
        let mut event = event_with_view(Some("published"), false);
        event.view = None; // arrived without view metadata

        assert!(
            build_event_payload(&event, &access, &runner, &registry, None).is_none(),
            "an event without view metadata must be dropped (fail-closed)"
        );
    }

    #[test]
    fn draft_event_delivered_when_draft_view_visible() {
        let (runner, registry, _posts) = build_runner_and_registry();

        let access = access_with_gate(EventViewGate {
            published: None,
            draft: Some(Vec::new()),
            trash: None,
        });
        let event = event_with_view(Some("draft"), false);

        assert!(
            build_event_payload(&event, &access, &runner, &registry, None).is_some(),
            "a draft-visible subscriber receives draft events"
        );
    }

    /// Views are independent: a draft-only reviewer does NOT see published events.
    #[test]
    fn published_event_dropped_when_only_draft_visible() {
        let (runner, registry, _posts) = build_runner_and_registry();

        let access = access_with_gate(EventViewGate {
            published: None,
            draft: Some(Vec::new()),
            trash: None,
        });
        let event = event_with_view(Some("published"), false);

        assert!(build_event_payload(&event, &access, &runner, &registry, None).is_none());
    }

    /// Soft-delete (trashed) events are gated by `trash`, not the status views.
    #[test]
    fn trashed_event_gated_by_trash_view() {
        let (runner, registry, _posts) = build_runner_and_registry();
        let event = event_with_view(Some("published"), true);

        // Published + draft visible but trash denied → withheld.
        let denied_trash = access_with_gate(EventViewGate {
            published: Some(Vec::new()),
            draft: Some(Vec::new()),
            trash: None,
        });
        assert!(
            build_event_payload(&event, &denied_trash, &runner, &registry, None).is_none(),
            "a trashed-doc event must require the trash view"
        );

        // Trash visible → delivered.
        let allow_trash = access_with_gate(EventViewGate {
            published: None,
            draft: None,
            trash: Some(Vec::new()),
        });
        assert!(
            build_event_payload(&event, &allow_trash, &runner, &registry, None).is_some(),
            "a trash-visible subscriber receives soft-delete events"
        );
    }
}
