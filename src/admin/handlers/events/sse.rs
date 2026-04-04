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
use tokio_stream::{Stream, StreamExt, wrappers::BroadcastStream};
use tokio_util::sync::WaitForCancellationFutureOwned;
use tracing::warn;

use crate::{
    admin::AdminState,
    core::{
        AuthUser, Document, Registry, Slug,
        collection::LiveMode,
        event::{EventOperation, EventTarget, MutationEvent},
    },
    db::{AccessResult, FilterClause, query::filter::memory},
    hooks::HookRunner,
};

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

/// Convert a mutation event to an SSE Event, applying access control, after_read hooks,
/// and field stripping to match normal read operations.
fn event_to_sse(
    event: &MutationEvent,
    access: &SseAccess,
    hook_runner: &HookRunner,
    registry: &Registry,
    user_doc: Option<&Document>,
) -> Option<Event> {
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

    let payload = json!({
        "sequence": event.sequence,
        "timestamp": event.timestamp,
        "target": target_str,
        "operation": op_str,
        "collection": event.collection,
        "document_id": event.document_id,
        "edited_by": event.edited_by,
        "data": data,
    });

    Some(
        Event::default()
            .event("mutation")
            .id(event.sequence.to_string())
            .data(payload.to_string()),
    )
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

    let event_bus = state.event_bus.clone();
    let shutdown = state.shutdown.clone();

    let user_doc = auth_user.as_ref().map(|ext| &ext.0.user_doc);
    let access = if event_bus.is_some() {
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

    let stream = if let Some(bus) = event_bus {
        let rx = bus.subscribe();

        let filtered = BroadcastStream::new(rx).filter_map(move |result| match result {
            Ok(event) => event_to_sse(
                &event,
                &access,
                &hook_runner,
                &registry,
                subscriber_user_doc.as_ref(),
            )
            .map(Ok::<_, Infallible>),
            Err(_) => None,
        });

        Box::pin(filtered) as Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>
    } else {
        Box::pin(tokio_stream::empty())
            as Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>
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
}
