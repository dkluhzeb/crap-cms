//! SSE endpoint for real-time mutation events in the admin UI.

use std::{
    collections::HashSet,
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

use axum::response::sse::{Event, KeepAlive, Sse};
use axum::{Extension, extract::State};
use serde_json::json;
use tokio_stream::{Stream, StreamExt, wrappers::BroadcastStream};
use tokio_util::sync::WaitForCancellationFutureOwned;

use crate::{
    admin::AdminState,
    core::{
        AuthUser, Slug,
        event::{EventOperation, EventTarget},
    },
    db::AccessResult,
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
    /// RAII guard — decrements SSE connection counter when the stream is dropped.
    _guard: Option<SseConnectionGuard>,
}

impl Stream for CancellableStream {
    type Item = Result<Event, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.done {
            return Poll::Ready(None);
        }
        // Check shutdown first
        if self.shutdown.as_mut().poll(cx).is_ready() {
            self.done = true;
            return Poll::Ready(None);
        }
        self.inner.as_mut().poll_next(cx)
    }
}

/// SSE handler — streams mutation events to authenticated admin users.
/// Auth user is injected by the admin middleware.
///
/// Excluded from tarpaulin: SSE streaming requires a persistent async connection
/// that cannot be tested via tower::oneshot (which completes immediately).
#[cfg_attr(not(tarpaulin_include), allow(dead_code))]
#[cfg(not(tarpaulin_include))]
pub async fn sse_handler(
    State(state): State<AdminState>,
    auth_user: Option<Extension<AuthUser>>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, axum::http::StatusCode> {
    // Enforce connection limit
    let max = state.max_sse_connections;
    if max > 0 {
        let current = state.sse_connections.fetch_add(1, Ordering::Relaxed);
        if current >= max {
            state.sse_connections.fetch_sub(1, Ordering::Relaxed);
            tracing::warn!(
                "SSE connection limit reached ({}/{}), rejecting",
                current,
                max
            );
            return Err(axum::http::StatusCode::SERVICE_UNAVAILABLE);
        }
    } else {
        state.sse_connections.fetch_add(1, Ordering::Relaxed);
    }

    let guard = SseConnectionGuard {
        counter: state.sse_connections.clone(),
    };

    let event_bus = state.event_bus.clone();
    let shutdown = state.shutdown.clone();

    // Build allowed collections/globals snapshot at subscribe time
    let mut allowed_collections: HashSet<Slug> = HashSet::new();
    let mut allowed_globals: HashSet<Slug> = HashSet::new();

    if let Some(ref bus) = event_bus {
        let _ = bus; // just to verify it exists
        {
            let user_doc = auth_user.as_ref().map(|ext| &ext.0.user_doc);

            if let Ok(mut conn) = state.pool.get()
                && let Ok(tx) = conn.transaction()
            {
                for (slug, def) in &state.registry.collections {
                    match state.hook_runner.check_access(
                        def.access.read.as_deref(),
                        user_doc,
                        None,
                        None,
                        &tx,
                    ) {
                        Ok(AccessResult::Allowed) | Ok(AccessResult::Constrained(_)) => {
                            allowed_collections.insert(slug.clone());
                        }
                        _ => {}
                    }
                }

                for (slug, def) in &state.registry.globals {
                    match state.hook_runner.check_access(
                        def.access.read.as_deref(),
                        user_doc,
                        None,
                        None,
                        &tx,
                    ) {
                        Ok(AccessResult::Allowed) | Ok(AccessResult::Constrained(_)) => {
                            allowed_globals.insert(slug.clone());
                        }
                        _ => {}
                    }
                }
                // Read-only access check — commit result is irrelevant, rollback on drop is safe
                let _ = tx.commit();
            }
        }
    }

    let stream = if let Some(bus) = event_bus {
        let rx = bus.subscribe();

        let filtered = BroadcastStream::new(rx).filter_map(move |result| {
            match result {
                Ok(event) => {
                    let allowed = match event.target {
                        EventTarget::Collection => allowed_collections.contains(&event.collection),
                        EventTarget::Global => allowed_globals.contains(&event.collection),
                    };
                    if !allowed {
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

                    let payload = json!({
                        "sequence": event.sequence,
                        "timestamp": event.timestamp,
                        "target": target_str,
                        "operation": op_str,
                        "collection": event.collection,
                        "document_id": event.document_id,
                        "edited_by": event.edited_by,
                    });

                    let sse_event = Event::default()
                        .event("mutation")
                        .id(event.sequence.to_string())
                        .data(payload.to_string());

                    Some(Ok::<_, Infallible>(sse_event))
                }
                Err(_) => None, // lagged — skip
            }
        });

        Box::pin(filtered) as Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>
    } else {
        // No event bus — return an empty stream that never yields
        let empty = tokio_stream::empty();

        Box::pin(empty) as Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>
    };

    // End the stream when the server is shutting down, so Axum's
    // graceful shutdown can complete without waiting for SSE clients.
    // The guard decrements the SSE connection counter when the stream is dropped.
    let stream = CancellableStream {
        inner: stream,
        shutdown: Box::pin(shutdown.cancelled_owned()),
        done: false,
        _guard: Some(guard),
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(30))))
}
