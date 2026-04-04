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
use tokio_stream::{Stream, StreamExt, wrappers::BroadcastStream};
use tonic::{Request, Response, Status};
use tracing::{error, warn};

use crate::{
    api::{
        content,
        service::{ContentService, convert::json_to_prost_value},
    },
    core::event::{EventOperation, EventTarget},
    db::{AccessResult, FilterClause, query::filter::memory::matches_constraints},
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

/// Resolved subscribe access: allowed slugs, denied fields, row-level constraints, modes, and user.
struct SubscribeAccess {
    allowed_collections: HashSet<String>,
    allowed_globals: HashSet<String>,
    denied_fields: HashMap<String, Vec<String>>,
    /// Row-level access constraints per collection (from `Constrained` access results).
    constraints: HashMap<String, Vec<FilterClause>>,
    /// Per-collection event delivery mode.
    modes: HashMap<String, crate::core::collection::LiveMode>,
    /// The subscriber's user document (for per-user after_read hooks).
    user_doc: Option<crate::core::Document>,
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
        let SubscribeAccess {
            allowed_collections,
            allowed_globals,
            denied_fields,
            constraints,
            modes,
            user_doc,
        } = access;

        let hook_runner = self.hook_runner.clone();
        let registry = self.registry.clone();

        let stream = BroadcastStream::new(rx).filter_map(move |result| match result {
            Ok(event) => {
                let allowed = match event.target {
                    EventTarget::Collection => {
                        allowed_collections.contains(event.collection.as_ref() as &str)
                    }
                    EventTarget::Global => {
                        allowed_globals.contains(event.collection.as_ref() as &str)
                    }
                };

                if !allowed {
                    return None;
                }

                let op_str = match event.operation {
                    EventOperation::Create => "create",
                    EventOperation::Update => "update",
                    EventOperation::Delete => "delete",
                };

                if !requested_ops.contains(op_str) {
                    return None;
                }

                // Row-level constraint check: skip events the subscriber can't access
                let slug_str: &str = event.collection.as_ref();

                if let Some(filters) = constraints.get(slug_str)
                    && !matches_constraints(&event.data, filters)
                {
                    return None;
                }

                // Apply mode: full = after_read hooks + data, metadata = no data
                use crate::core::collection::LiveMode;

                let mode = modes.get(slug_str).copied().unwrap_or_default();

                let fields: BTreeMap<String, prost_types::Value> = if mode == LiveMode::Full {
                    // Run after_read hooks to transform data (same as Find)
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
                        user_doc.as_ref(),
                    );

                    // Strip field-level read-denied fields
                    let denied = denied_fields.get(slug_str);

                    processed_data
                        .iter()
                        .filter(|(k, _)| denied.is_none_or(|d| !d.iter().any(|name| name == *k)))
                        .map(|(k, v)| (k.clone(), json_to_prost_value(v)))
                        .collect()
                } else {
                    BTreeMap::new() // metadata mode: no data
                };

                let target_str = match event.target {
                    EventTarget::Collection => "collection",
                    EventTarget::Global => "global",
                };

                Some(Ok(content::MutationEvent {
                    sequence: event.sequence,
                    timestamp: event.timestamp,
                    target: target_str.to_string(),
                    operation: op_str.to_string(),
                    collection: event.collection.to_string(),
                    document_id: event.document_id.to_string(),
                    data: Some(prost_types::Struct { fields }),
                }))
            }
            Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
                tracing::warn!("Subscribe stream lagged by {} events", n);
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

            let target_collections: Vec<String> = if collections_req.is_empty() {
                registry.collections.keys().map(|s| s.to_string()).collect()
            } else {
                collections_req
            };

            let mut allowed_collections: HashSet<String> = HashSet::new();
            let mut denied_fields: HashMap<String, Vec<String>> = HashMap::new();
            let mut constraints: HashMap<String, Vec<FilterClause>> = HashMap::new();
            let mut modes: HashMap<String, crate::core::collection::LiveMode> = HashMap::new();

            for slug in &target_collections {
                let Some(def) = registry.get_collection(slug) else {
                    continue;
                };

                match hook_runner.check_access(
                    def.access.read.as_deref(),
                    user_doc,
                    None,
                    None,
                    &tx,
                ) {
                    Ok(AccessResult::Allowed) => {
                        allowed_collections.insert(slug.clone());
                    }
                    Ok(AccessResult::Constrained(filters)) => {
                        allowed_collections.insert(slug.clone());
                        constraints.insert(slug.clone(), filters);
                    }
                    _ => continue,
                }

                let denied = hook_runner.check_field_read_access(&def.fields, user_doc, &tx);

                if !denied.is_empty() {
                    denied_fields.insert(slug.clone(), denied);
                }

                modes.insert(slug.clone(), def.live_mode);
            }

            let target_globals: Vec<String> = if globals_req.is_empty() {
                registry.globals.keys().map(|s| s.to_string()).collect()
            } else {
                globals_req
            };

            let mut allowed_globals: HashSet<String> = HashSet::new();

            for slug in &target_globals {
                let Some(def) = registry.get_global(slug) else {
                    continue;
                };

                match hook_runner.check_access(
                    def.access.read.as_deref(),
                    user_doc,
                    None,
                    None,
                    &tx,
                ) {
                    Ok(AccessResult::Allowed) => {
                        allowed_globals.insert(slug.clone());
                    }
                    Ok(AccessResult::Constrained(filters)) => {
                        allowed_globals.insert(slug.clone());
                        constraints.insert(slug.clone(), filters);
                    }
                    _ => continue,
                }

                let denied = hook_runner.check_field_read_access(&def.fields, user_doc, &tx);

                if !denied.is_empty() {
                    denied_fields.insert(slug.clone(), denied);
                }

                modes.insert(slug.clone(), def.live_mode);
            }

            if let Err(e) = tx.commit() {
                warn!("tx commit failed: {e}");
            }

            let user_doc = auth_user.map(|au| au.user_doc);

            Ok(SubscribeAccess {
                allowed_collections,
                allowed_globals,
                denied_fields,
                modes,
                constraints,
                user_doc,
            })
        })
        .await
        .map_err(|e| {
            error!("Subscribe task error: {}", e);
            Status::internal("Internal error")
        })?
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
