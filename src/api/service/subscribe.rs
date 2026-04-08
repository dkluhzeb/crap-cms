//! Subscribe handler — real-time mutation event streaming.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

use tokio::task;
use tokio_stream::{
    Stream, StreamExt,
    wrappers::{BroadcastStream, errors::BroadcastStreamRecvError},
};
use tonic::{Request, Response, Status};
use tracing::{error, warn};

use crate::{
    api::{
        content,
        service::{ContentService, convert::json_to_prost_value},
    },
    core::{
        Document, FieldDefinition, Registry,
        collection::LiveMode,
        event::MutationEvent,
        event::{EventOperation, EventTarget},
    },
    db::{AccessResult, DbConnection, FilterClause, query::filter::memory::matches_constraints},
    hooks::HookRunner,
};

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

/// Per-slug access resolution result.
struct SlugAccess {
    access_ref: Option<String>,
    fields: Vec<FieldDefinition>,
    live_mode: LiveMode,
}

/// Accumulated access state built during slug resolution.
struct AccessState {
    allowed: HashSet<String>,
    denied_fields: HashMap<String, Vec<String>>,
    constraints: HashMap<String, Vec<FilterClause>>,
    modes: HashMap<String, LiveMode>,
}

impl AccessState {
    fn new() -> Self {
        Self {
            allowed: HashSet::new(),
            denied_fields: HashMap::new(),
            constraints: HashMap::new(),
            modes: HashMap::new(),
        }
    }
}

/// Resolve access for a single slug: check access, cache denied fields, constraints, and mode.
fn resolve_single_slug(
    slug: &str,
    slug_access: &SlugAccess,
    user_doc: Option<&Document>,
    hook_runner: &HookRunner,
    tx: &dyn DbConnection,
    state: &mut AccessState,
) {
    match hook_runner.check_access(slug_access.access_ref.as_deref(), user_doc, None, None, tx) {
        Ok(AccessResult::Allowed) => {
            state.allowed.insert(slug.to_string());
        }
        Ok(AccessResult::Constrained(filters)) => {
            state.allowed.insert(slug.to_string());
            state.constraints.insert(slug.to_string(), filters);
        }
        _ => return,
    }

    let denied = hook_runner.check_field_read_access(&slug_access.fields, user_doc, tx);

    if !denied.is_empty() {
        state.denied_fields.insert(slug.to_string(), denied);
    }

    state.modes.insert(slug.to_string(), slug_access.live_mode);
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

    let allowed = match event.target {
        EventTarget::Collection => ctx.access.allowed_collections.contains(slug_str),
        EventTarget::Global => ctx.access.allowed_globals.contains(slug_str),
    };

    if !allowed {
        return None;
    }

    let op_str = match event.operation {
        EventOperation::Create => "create",
        EventOperation::Update => "update",
        EventOperation::Delete => "delete",
    };

    if !ctx.requested_ops.contains(op_str) {
        return None;
    }

    if let Some(filters) = ctx.access.constraints.get(slug_str)
        && !matches_constraints(&event.data, filters)
    {
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

        let processed = ctx.hook_runner.apply_after_read_for_event(
            slug_str,
            &hooks,
            &field_defs,
            event.document_id.as_ref(),
            &event.data,
            ctx.access.user_doc.as_ref(),
        );

        let denied = ctx.access.denied_fields.get(slug_str);

        processed
            .iter()
            .filter(|(k, _)| denied.is_none_or(|d| !d.iter().any(|name| name == *k)))
            .map(|(k, v)| (k.clone(), json_to_prost_value(v)))
            .collect()
    } else {
        BTreeMap::new()
    };

    let target_str = match event.target {
        EventTarget::Collection => "collection",
        EventTarget::Global => "global",
    };

    Some(content::MutationEvent {
        sequence: event.sequence,
        timestamp: event.timestamp.clone(),
        target: target_str.to_string(),
        operation: op_str.to_string(),
        collection: event.collection.to_string(),
        document_id: event.document_id.to_string(),
        data: Some(prost_types::Struct { fields }),
    })
}

/// Resolved subscribe access: allowed slugs, denied fields, row-level constraints, modes, and user.
struct SubscribeAccess {
    allowed_collections: HashSet<String>,
    allowed_globals: HashSet<String>,
    denied_fields: HashMap<String, Vec<String>>,
    /// Row-level access constraints per collection (from `Constrained` access results).
    constraints: HashMap<String, Vec<FilterClause>>,
    /// Per-collection event delivery mode.
    modes: HashMap<String, LiveMode>,
    /// The subscriber's user document (for per-user after_read hooks).
    user_doc: Option<Document>,
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Subscribe to real-time mutation events (server streaming).
    pub(in crate::api::service) async fn subscribe_impl(
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

        let event_bus = self
            .event_bus
            .as_ref()
            .ok_or_else(|| Status::unavailable("Live updates disabled"))?;

        let token = Self::extract_token(&metadata);

        let requested_ops: HashSet<String> = if req.operations.is_empty() {
            ["create", "update", "delete"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        } else {
            req.operations.into_iter().collect()
        };

        let access = self
            .resolve_subscribe_access(token, req.collections, req.globals)
            .await?;

        if access.allowed_collections.is_empty() && access.allowed_globals.is_empty() {
            return Err(Status::permission_denied(
                "No accessible collections or globals",
            ));
        }

        let rx = event_bus.subscribe();

        let subscriber = SubscriberCtx {
            access,
            requested_ops,
            hook_runner: self.hook_runner.clone(),
            registry: self.registry.clone(),
        };

        let stream = BroadcastStream::new(rx).filter_map(move |result| match result {
            Ok(event) => process_event(&event, &subscriber).map(Ok),
            Err(BroadcastStreamRecvError::Lagged(n)) => {
                warn!("Subscribe stream lagged by {} events", n);

                None
            }
        });

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
        collections_req: Vec<String>,
        globals_req: Vec<String>,
    ) -> Result<SubscribeAccess, Status> {
        let pool = self.pool.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let hook_runner = self.hook_runner.clone();

        task::spawn_blocking(move || {
            let mut conn = pool.get().map_err(|e| {
                error!("Subscribe pool error: {}", e);

                Status::internal("Internal error")
            })?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;
            let user_doc = auth_user.as_ref().map(|u| &u.user_doc);

            let tx = conn.transaction().map_err(|e| {
                error!("Subscribe tx error: {}", e);

                Status::internal("Internal error")
            })?;

            let mut col_state = AccessState::new();
            let mut global_state = AccessState::new();

            let target_collections: Vec<String> = if collections_req.is_empty() {
                registry.collections.keys().map(|s| s.to_string()).collect()
            } else {
                collections_req
            };

            for slug in &target_collections {
                if let Some(def) = registry.get_collection(slug) {
                    resolve_single_slug(
                        slug,
                        &SlugAccess {
                            access_ref: def.access.read.clone(),
                            fields: def.fields.clone(),
                            live_mode: def.live_mode,
                        },
                        user_doc,
                        &hook_runner,
                        &tx,
                        &mut col_state,
                    );
                }
            }

            let target_globals: Vec<String> = if globals_req.is_empty() {
                registry.globals.keys().map(|s| s.to_string()).collect()
            } else {
                globals_req
            };

            for slug in &target_globals {
                if let Some(def) = registry.get_global(slug) {
                    resolve_single_slug(
                        slug,
                        &SlugAccess {
                            access_ref: def.access.read.clone(),
                            fields: def.fields.clone(),
                            live_mode: def.live_mode,
                        },
                        user_doc,
                        &hook_runner,
                        &tx,
                        &mut global_state,
                    );
                }
            }

            if let Err(e) = tx.commit() {
                warn!("tx commit failed: {e}");
            }

            // Merge denied_fields, constraints, and modes (globals share the same maps)
            let mut denied_fields = col_state.denied_fields;
            denied_fields.extend(global_state.denied_fields);
            let mut constraints = col_state.constraints;
            constraints.extend(global_state.constraints);
            let mut modes = col_state.modes;
            modes.extend(global_state.modes);

            Ok(SubscribeAccess {
                allowed_collections: col_state.allowed,
                allowed_globals: global_state.allowed,
                denied_fields,
                modes,
                constraints,
                user_doc: auth_user.map(|au| au.user_doc),
            })
        })
        .await
        .inspect_err(|e| error!("Subscribe task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))?
    }
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
