use axum::{
    Extension,
    extract::{Path, State},
    response::{IntoResponse, Response},
};
use tokio::task;
use tracing::error;

use crate::{
    admin::{
        AdminState,
        handlers::shared::{forbidden, get_user_doc, htmx_redirect, paths, redirect_response},
    },
    config::LocaleConfig,
    core::{
        CollectionDefinition, Document, SharedCache, SharedEventTransport,
        SharedInvalidationTransport, auth::AuthUser,
    },
    db::DbPool,
    hooks::HookRunner,
    service::{ServiceContext, ServiceError, restore_collection_version},
};

/// Owned inputs for the spawn-blocking restore body.
struct RestoreVersionInput {
    pool: DbPool,
    runner: HookRunner,
    slug: String,
    def: CollectionDefinition,
    user_doc: Option<Document>,
    event_transport: Option<SharedEventTransport>,
    invalidation_transport: SharedInvalidationTransport,
    cache: Option<SharedCache>,
    id: String,
    version_id: String,
    locale_config: LocaleConfig,
}

/// Build the service context and run the version-restore service call. Wraps
/// the inline closure body so the `spawn_blocking` call is a single fn
/// invocation per CLAUDE.md.
fn restore_collection_version_blocking(
    input: RestoreVersionInput,
) -> Result<Document, ServiceError> {
    let ctx = ServiceContext::collection(&input.slug, &input.def)
        .pool(&input.pool)
        .runner(&input.runner)
        .user(input.user_doc.as_ref())
        .event_transport(input.event_transport)
        .invalidation_transport(Some(input.invalidation_transport))
        .cache(input.cache)
        .build();

    restore_collection_version(&ctx, &input.id, &input.version_id, &input.locale_config)
}

/// `POST /admin/collections/{slug}/{id}/versions/{version_id}/restore` — restore a version
pub async fn restore_version(
    State(state): State<AdminState>,
    Path((slug, id, version_id)): Path<(String, String, String)>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let Some(def) = state.registry.get_collection(&slug).cloned() else {
        return redirect_response(paths::COLLECTIONS_ROOT);
    };

    if !def.has_versions() {
        return redirect_response(&paths::collection_item(&slug, &id));
    }

    let redirect = paths::collection_item(&slug, &id);
    let input = RestoreVersionInput {
        pool: state.pool.clone(),
        runner: state.hook_runner.clone(),
        slug,
        def,
        user_doc: get_user_doc(auth_user.as_ref()).cloned(),
        event_transport: state.event_transport.clone(),
        invalidation_transport: state.invalidation_transport.clone(),
        cache: state.cache.clone(),
        id,
        version_id,
        locale_config: state.config.locale.clone(),
    };

    let result = task::spawn_blocking(move || restore_collection_version_blocking(input)).await;

    match result {
        Ok(Ok(_)) => htmx_redirect(&redirect),
        Ok(Err(ServiceError::AccessDenied(_))) => {
            forbidden(&state, "You don't have permission to restore this version").into_response()
        }
        Ok(Err(e)) => {
            error!("Restore version error: {}", e);

            htmx_redirect(&redirect)
        }
        Err(e) => {
            error!("Restore version task error: {}", e);

            htmx_redirect(&redirect)
        }
    }
}
