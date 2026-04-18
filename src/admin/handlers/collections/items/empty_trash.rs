//! POST /admin/collections/{slug}/empty-trash — permanently delete all trashed documents.

use axum::{
    Extension,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use serde_json::json;
use tokio::task;
use tracing::error;

use crate::{
    admin::AdminState,
    config::LocaleConfig,
    core::{
        CollectionDefinition, Document,
        auth::AuthUser,
        cache::SharedCache,
        event::{SharedEventTransport, SharedInvalidationTransport},
        upload,
        upload::StorageBackend,
    },
    db::{DbPool, Filter, FilterClause, FilterOp},
    hooks::HookRunner,
    service::{DeleteManyOptions, ServiceContext, ServiceError, delete_many},
};

/// Build trash filters: match only soft-deleted documents.
fn trash_filters() -> Vec<FilterClause> {
    vec![FilterClause::Single(Filter {
        field: "_deleted_at".to_string(),
        op: FilterOp::Exists,
    })]
}

/// Find all trashed documents and permanently delete them via the service layer.
#[allow(clippy::too_many_arguments)]
fn empty_trash(
    pool: &DbPool,
    runner: &HookRunner,
    def: &CollectionDefinition,
    slug: &str,
    locale_cfg: &LocaleConfig,
    storage: &dyn StorageBackend,
    user_doc: Option<&Document>,
    invalidation_transport: Option<SharedInvalidationTransport>,
    event_transport: Option<SharedEventTransport>,
    cache: Option<SharedCache>,
) -> Result<usize, ServiceError> {
    let mut hard_def = def.clone();
    hard_def.soft_delete = false;

    let filters = trash_filters();

    let ctx = ServiceContext::collection(slug, &hard_def)
        .pool(pool)
        .runner(runner)
        .user(user_doc)
        .invalidation_transport(invalidation_transport)
        .event_transport(event_transport)
        .cache(cache)
        .build();

    let delete_opts = DeleteManyOptions {
        run_hooks: true,
        include_deleted: true,
    };

    let result = delete_many(&ctx, filters, locale_cfg, &delete_opts)?;

    for fields in &result.upload_fields_to_clean {
        upload::delete_upload_files(storage, fields);
    }

    Ok(result.hard_deleted as usize)
}

/// POST /admin/collections/{slug}/empty-trash
#[cfg(not(tarpaulin_include))]
pub async fn empty_trash_action(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => return StatusCode::NOT_FOUND.into_response(),
    };

    if !def.soft_delete {
        return (
            StatusCode::BAD_REQUEST,
            "Collection does not support soft delete",
        )
            .into_response();
    }

    let pool = state.pool.clone();
    let storage = state.storage.clone();
    let locale_cfg = state.config.locale.clone();
    let runner = state.hook_runner.clone();
    let user_doc = auth_user.as_ref().map(|Extension(au)| au.user_doc.clone());
    let invalidation_transport = state.invalidation_transport.clone();
    let event_transport = state.event_transport.clone();
    let cache = state.cache.clone();

    let result = task::spawn_blocking(move || {
        empty_trash(
            &pool,
            &runner,
            &def,
            &slug,
            &locale_cfg,
            &*storage,
            user_doc.as_ref(),
            Some(invalidation_transport),
            event_transport,
            cache,
        )
    })
    .await;

    match result {
        Ok(Ok(count)) => Json(json!({"ok": true, "count": count})).into_response(),
        Ok(Err(ServiceError::AccessDenied(_))) => StatusCode::FORBIDDEN.into_response(),
        Ok(Err(e)) => {
            error!("Empty trash error: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Failed to empty trash"})),
            )
                .into_response()
        }
        Err(e) => {
            error!("Empty trash task error: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Internal error"})),
            )
                .into_response()
        }
    }
}
