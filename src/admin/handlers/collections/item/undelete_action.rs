//! POST /admin/collections/{slug}/{id}/undelete — undelete a soft-deleted document.

use axum::{
    Extension,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use tokio::task;
use tracing::{error, info};

use crate::{
    admin::{
        AdminState,
        handlers::shared::{forbidden, get_user_doc, htmx_redirect, paths},
    },
    core::{CollectionDefinition, Document, SharedCache, SharedEventTransport, auth::AuthUser},
    db::DbPool,
    hooks::HookRunner,
    service::{self, ServiceContext, ServiceError},
};

/// Owned inputs for the spawn-blocking undelete body.
struct UndeleteInput {
    pool: DbPool,
    runner: HookRunner,
    slug: String,
    def: CollectionDefinition,
    user_doc: Option<Document>,
    event_transport: Option<SharedEventTransport>,
    cache: Option<SharedCache>,
    id: String,
}

/// Build the service context and run the undelete service call.
fn undelete_document_blocking(input: UndeleteInput) -> Result<Document, ServiceError> {
    let ctx = ServiceContext::collection(&input.slug, &input.def)
        .pool(&input.pool)
        .runner(&input.runner)
        .user(input.user_doc.as_ref())
        .event_transport(input.event_transport)
        .cache(input.cache)
        .build();

    service::undelete_document(&ctx, &input.id)
}

/// POST /admin/collections/{slug}/{id}/undelete — undelete a soft-deleted item
pub async fn undelete_action(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let Some(def) = state.registry.get_collection(&slug).cloned() else {
        return htmx_redirect(paths::COLLECTIONS_ROOT);
    };

    if !def.soft_delete {
        return htmx_redirect(&paths::collection(&slug));
    }

    let input = UndeleteInput {
        pool: state.pool.clone(),
        runner: state.hook_runner.clone(),
        slug: slug.clone(),
        def,
        user_doc: get_user_doc(&auth_user).cloned(),
        event_transport: state.event_transport.clone(),
        cache: state.cache.clone(),
        id: id.clone(),
    };

    let result = task::spawn_blocking(move || undelete_document_blocking(input)).await;

    match result {
        Ok(Ok(_doc)) => {
            info!("Undeleted document {} in {}", id, slug);

            htmx_redirect(&paths::collection_trash(&slug))
        }
        Ok(Err(ServiceError::AccessDenied(_))) => {
            forbidden(&state, "You don't have permission to undelete this item").into_response()
        }
        Ok(Err(e)) => {
            error!("Undelete error: {}", e);

            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Undelete failed: {}", e),
            )
                .into_response()
        }
        Err(e) => {
            error!("Undelete task error: {}", e);

            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Undelete failed: {}", e),
            )
                .into_response()
        }
    }
}
