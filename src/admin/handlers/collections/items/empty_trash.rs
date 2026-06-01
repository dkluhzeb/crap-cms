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
        AuthUser, CollectionDefinition, Document, SharedCache, SharedEventTransport,
        SharedInvalidationTransport, upload, upload::StorageBackend,
    },
    db::{DbPool, Filter, FilterClause, FilterOp},
    hooks::HookRunner,
    service::{DeleteManyOptions, ServiceContext, ServiceError, delete_many},
};

/// Bundled inputs for [`empty_trash`]. Grouped to keep the spawn-blocking
/// closure tidy and to satisfy the `>4 args` rule from CLAUDE.md.
struct EmptyTrashInput<'a> {
    pool: &'a DbPool,
    runner: &'a HookRunner,
    def: &'a CollectionDefinition,
    slug: &'a str,
    locale_cfg: &'a LocaleConfig,
    storage: &'a dyn StorageBackend,
    user_doc: Option<&'a Document>,
    invalidation_transport: Option<SharedInvalidationTransport>,
    event_transport: Option<SharedEventTransport>,
    cache: Option<SharedCache>,
}

/// Build trash filters: match only soft-deleted documents.
fn trash_filters() -> Vec<FilterClause> {
    vec![FilterClause::Single(Filter {
        field: "_deleted_at".to_string(),
        op: FilterOp::Exists,
    })]
}

/// Find all trashed documents and permanently delete them via the service layer.
fn empty_trash(input: EmptyTrashInput<'_>) -> Result<usize, ServiceError> {
    let mut hard_def = input.def.clone();
    hard_def.soft_delete = false;

    let filters = trash_filters();

    let ctx = ServiceContext::collection(input.slug, &hard_def)
        .pool(input.pool)
        .runner(input.runner)
        .user(input.user_doc)
        .invalidation_transport(input.invalidation_transport)
        .event_transport(input.event_transport)
        .cache(input.cache)
        .build();

    let delete_opts = DeleteManyOptions {
        run_hooks: true,
        include_deleted: true,
    };

    let result = delete_many(&ctx, &filters, input.locale_cfg, &delete_opts)?;

    for fields in &result.upload_fields_to_clean {
        upload::delete_upload_files(input.storage, fields);
    }

    Ok(usize::try_from(result.hard_deleted.max(0)).unwrap_or(0))
}

/// POST /admin/collections/{slug}/empty-trash
#[cfg(not(tarpaulin_include))]
pub async fn empty_trash_action(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let Some(def) = state.registry.get_collection(&slug).cloned() else {
        return StatusCode::NOT_FOUND.into_response();
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
        empty_trash(EmptyTrashInput {
            pool: &pool,
            runner: &runner,
            def: &def,
            slug: &slug,
            locale_cfg: &locale_cfg,
            storage: &*storage,
            user_doc: user_doc.as_ref(),
            invalidation_transport: Some(invalidation_transport),
            event_transport,
            cache,
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trash_filters_select_only_soft_deleted_rows() {
        let filters = trash_filters();
        assert_eq!(filters.len(), 1);
        assert!(matches!(
            &filters[0],
            FilterClause::Single(f) if f.field == "_deleted_at" && matches!(f.op, FilterOp::Exists)
        ));
    }
}
