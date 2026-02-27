//! SSE endpoint for real-time mutation events in the admin UI.

use std::collections::HashSet;
use std::convert::Infallible;
use std::time::Duration;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use tokio_stream::{Stream, StreamExt, wrappers::BroadcastStream};

use crate::core::auth::AuthUser;
use crate::db::query::AccessResult;
use super::super::AdminState;

/// SSE handler — streams mutation events to authenticated admin users.
/// Auth user is injected by the admin middleware.
///
/// Excluded from tarpaulin: SSE streaming requires a persistent async connection
/// that cannot be tested via tower::oneshot (which completes immediately).
#[cfg_attr(not(tarpaulin_include), allow(dead_code))]
#[cfg(not(tarpaulin_include))]
pub async fn sse_handler(
    State(state): State<AdminState>,
    auth_user: Option<axum::Extension<AuthUser>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let event_bus = state.event_bus.clone();

    // Build allowed collections/globals snapshot at subscribe time
    let mut allowed_collections: HashSet<String> = HashSet::new();
    let mut allowed_globals: HashSet<String> = HashSet::new();

    if let Some(ref bus) = event_bus {
        let _ = bus; // just to verify it exists
        let reg = state.registry.read().ok();
        if let Some(reg) = reg {
            let user_doc = auth_user.as_ref().map(|ext| &ext.0.user_doc);

            if let Ok(conn) = state.pool.get() {
                for (slug, def) in &reg.collections {
                    match state.hook_runner.check_access(
                        def.access.read.as_deref(), user_doc, None, None, &conn,
                    ) {
                        Ok(AccessResult::Allowed) | Ok(AccessResult::Constrained(_)) => {
                            allowed_collections.insert(slug.clone());
                        }
                        _ => {}
                    }
                }

                for (slug, def) in &reg.globals {
                    match state.hook_runner.check_access(
                        def.access.read.as_deref(), user_doc, None, None, &conn,
                    ) {
                        Ok(AccessResult::Allowed) | Ok(AccessResult::Constrained(_)) => {
                            allowed_globals.insert(slug.clone());
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    let stream = if let Some(bus) = event_bus {
        let rx = bus.subscribe();
        let filtered = BroadcastStream::new(rx)
            .filter_map(move |result| {
                match result {
                    Ok(event) => {
                        let allowed = match event.target {
                            crate::core::event::EventTarget::Collection => {
                                allowed_collections.contains(&event.collection)
                            }
                            crate::core::event::EventTarget::Global => {
                                allowed_globals.contains(&event.collection)
                            }
                        };
                        if !allowed {
                            return None;
                        }

                        let target_str = match event.target {
                            crate::core::event::EventTarget::Collection => "collection",
                            crate::core::event::EventTarget::Global => "global",
                        };
                        let op_str = match event.operation {
                            crate::core::event::EventOperation::Create => "create",
                            crate::core::event::EventOperation::Update => "update",
                            crate::core::event::EventOperation::Delete => "delete",
                        };

                        let payload = serde_json::json!({
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
        Box::pin(filtered) as std::pin::Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>
    } else {
        // No event bus — return an empty stream that never yields
        let empty = tokio_stream::empty();
        Box::pin(empty) as std::pin::Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>
    };

    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(30)))
}
