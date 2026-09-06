//! SSE endpoint for real-time mutation events in the admin UI.
//!
//! This module owns the connection/streaming plumbing (slot limiting, the
//! per-subscriber pump task, the cancellable stream, the route handler). The
//! access resolution and JSON payload construction live in [`super::sse_payload`].

use std::{
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
use tokio::{sync::mpsc, time::timeout};
use tokio_stream::{Stream, wrappers::ReceiverStream};
use tokio_util::sync::WaitForCancellationFutureOwned;
use tracing::warn;

use crate::admin::handlers::shared::response::on_blocking_section;
use crate::{
    admin::{
        AdminState,
        handlers::events::sse_payload::{SseAccess, build_allowed_slugs, event_to_sse},
    },
    core::{
        AuthUser, Document, EventReceiver, MutationEvent, Registry,
        event::{InvalidationReceiver, MAX_DRAIN, RecvError, drain_and_coalesce},
    },
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

/// Handle one event recv result: drain-and-coalesce everything already
/// queued (bursts collapse latest-wins per document, so the gate +
/// `after_read` pipeline runs once per document), then forward the
/// survivors. Returns `Err(())` if the subscriber should be dropped.
async fn handle_broadcast_recv(
    tx: &mpsc::Sender<Result<Event, Infallible>>,
    ctx: &PumpCtx,
    recv: Result<MutationEvent, RecvError>,
    event_rx: &mut EventReceiver,
) -> Result<(), ()> {
    let event = match recv {
        Ok(event) => event,
        Err(RecvError::Lagged(n)) => {
            warn_sse_lag(n);
            return Err(());
        }
        Err(RecvError::Closed) => return Err(()),
    };

    // Admin SSE delivers every operation (no per-op scoping), so keep all.
    let outcome = drain_and_coalesce(event, event_rx, MAX_DRAIN, |_| true);

    // Build the SSE payloads for the whole drained batch in ONE blocking
    // hop: `event_to_sse` runs the per-event field-read
    // strip and `after_read` hooks, which acquire a Lua VM (up to 5s) —
    // that must not run on the async pump worker. Forwarding stays async.
    let sse_events = on_blocking_section(|| {
        outcome
            .events
            .iter()
            .filter_map(|event| {
                event_to_sse(
                    event,
                    &ctx.access,
                    &ctx.hook_runner,
                    &ctx.registry,
                    ctx.user_doc.as_ref(),
                )
            })
            .collect::<Vec<_>>()
    });

    for sse_event in sse_events {
        forward_event(tx, sse_event, ctx.send_timeout).await?;
    }

    // Mid-sweep lag/close: survivors were delivered; apply the same
    // fail-safe as a lagged recv — drop the subscriber.
    if let Some(n) = outcome.lagged {
        warn_sse_lag(n);
        return Err(());
    }
    if outcome.closed {
        return Err(());
    }

    Ok(())
}

fn warn_sse_lag(n: u64) {
    warn!(
        "SSE subscriber lagged by {} events — dropping client (forces reconnect)",
        n
    );
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
                    if handle_broadcast_recv(&tx, &ctx, recv, &mut event_rx).await.is_err() {
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

    let event_transport = state.infra.event_transport.clone();
    let shutdown = state.shutdown.clone();

    let user_doc = auth_user.as_ref().map(|ext| &ext.0.user_doc);
    let access = if event_transport.is_some() {
        build_allowed_slugs(&state, user_doc)
    } else {
        SseAccess::empty()
    };

    let hook_runner = state.infra.hook_runner.clone();
    let registry = Arc::clone(&state.infra.registry);
    let subscriber_user_doc = auth_user.as_ref().map(|ext| ext.0.user_doc.clone());
    let subscriber_user_id = auth_user.as_ref().map(|ext| ext.0.claims.sub.to_string());
    let send_timeout = Duration::from_millis(state.subscriber_send_timeout_ms);
    let invalidation_rx = state.infra.invalidation_transport.subscribe();

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
