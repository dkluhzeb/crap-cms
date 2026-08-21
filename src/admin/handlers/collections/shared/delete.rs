//! Delete handler — soft-delete or permanent deletion of collection items.

use axum::{
    Extension, Json,
    response::{IntoResponse, Redirect, Response},
};
use serde_json::json;
use tokio::task;
use tracing::error;

use crate::{
    admin::{
        AdminState,
        handlers::shared::{
            forbidden, get_user_doc, htmx_redirect, json_conflict, json_forbidden, json_not_found,
            json_server_error, paths,
        },
    },
    config::LocaleConfig,
    core::ReqContext,
    core::{
        AuthUser, CollectionDefinition, Document, SharedCache, SharedEventTransport,
        SharedInvalidationTransport, SharedStorage,
    },
    db::DbPool,
    hooks::HookRunner,
    service::{self, ServiceError},
};

/// Owned inputs for the spawn-blocking delete body.
struct DeleteBlockingInput {
    pool: DbPool,
    runner: HookRunner,
    slug: String,
    def: CollectionDefinition,
    user_doc: Option<Document>,
    invalidation_transport: SharedInvalidationTransport,
    event_transport: Option<SharedEventTransport>,
    cache: Option<SharedCache>,
    storage: SharedStorage,
    id: String,
    locale_config: LocaleConfig,
}

/// Build the service context and run the delete service call.
fn delete_document_blocking(input: DeleteBlockingInput) -> Result<ReqContext, ServiceError> {
    let ctx = service::ServiceContext::collection(&input.slug, &input.def)
        .pool(&input.pool)
        .runner(&input.runner)
        .user(input.user_doc.as_ref())
        .invalidation_transport(Some(input.invalidation_transport))
        .event_transport(input.event_transport)
        .cache(input.cache)
        .build();

    service::delete_document(
        &ctx,
        &input.id,
        Some(&*input.storage),
        Some(&input.locale_config),
    )
}

/// Build a JSON `{"ok": true}` success response.
fn json_ok_response() -> Response {
    Json(json!({"ok": true})).into_response()
}

/// DELETE handler for collection items (called from `delete_action.rs`).
pub(in crate::admin::handlers::collections) async fn delete_action_impl(
    state: &AdminState,
    slug: &str,
    id: &str,
    auth_user: Option<&Extension<AuthUser>>,
    force_hard_delete: bool,
    json_response: bool,
) -> Response {
    let Some(def) = state.registry.get_collection(slug).cloned() else {
        if json_response {
            return json_not_found("Collection not found");
        }

        return Redirect::to(paths::COLLECTIONS_ROOT).into_response();
    };

    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let mut def_clone = def.clone();
    let slug_owned = slug.to_string();
    let id_owned = id.to_string();
    let user_doc = get_user_doc(auth_user).cloned();
    let storage = state.storage.clone();
    let locale_config = state.config.locale.clone();
    let invalidation_transport = state.invalidation_transport.clone();
    let event_transport = state.event_transport.clone();
    let cache = state.cache.clone();

    if force_hard_delete {
        def_clone.make_hard_delete();
    }

    let input = DeleteBlockingInput {
        pool,
        runner,
        slug: slug_owned,
        def: def_clone,
        user_doc,
        invalidation_transport,
        event_transport,
        cache,
        storage,
        id: id_owned,
        locale_config,
    };

    let result = task::spawn_blocking(move || delete_document_blocking(input)).await;

    match result {
        Ok(Ok(_)) => {
            if json_response {
                return json_ok_response();
            }
        }
        Ok(Err(e)) => match &e {
            ServiceError::AccessDenied(_) => {
                let deny_msg = if def.soft_delete && !force_hard_delete {
                    "You don't have permission to trash this item"
                } else {
                    "You don't have permission to permanently delete this item"
                };

                if json_response {
                    return json_forbidden(deny_msg);
                }

                return forbidden(state, deny_msg).into_response();
            }
            ServiceError::Referenced { count, .. } => {
                // A precondition conflict, not a client input error.
                if json_response {
                    return json_conflict(&format!(
                        "Cannot delete: referenced by {count} document(s)"
                    ));
                }
            }
            _ => {
                error!("Delete error: {}", e);

                if json_response {
                    return json_server_error("Failed to delete item");
                }
            }
        },
        Err(e) => {
            error!("Delete task error: {}", e);

            if json_response {
                return json_server_error("Failed to delete item");
            }
        }
    }

    htmx_redirect(&paths::collection(slug))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::HookRef;
    use crate::core::collection::Access;
    use axum::http::StatusCode;

    #[test]
    fn trash_access_falls_back_to_update() {
        let access = Access {
            trash: Some(HookRef::new("access.trash_fn")),
            update: Some(HookRef::new("access.update_fn")),
            ..Default::default()
        };
        assert_eq!(
            access.resolve_trash(),
            Some(&HookRef::new("access.trash_fn"))
        );

        let access = Access {
            trash: None,
            update: Some(HookRef::new("access.update_fn")),
            ..Default::default()
        };
        assert_eq!(
            access.resolve_trash(),
            Some(&HookRef::new("access.update_fn"))
        );

        let access = Access::default();
        assert!(access.resolve_trash().is_none());
    }

    #[test]
    fn json_ok_response_returns_200() {
        let resp = json_ok_response();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
