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
        CollectionDefinition, auth::AuthUser, event::SharedInvalidationTransport, upload,
        upload::StorageBackend,
    },
    db::{DbPool, FindQuery},
    hooks::HookRunner,
    service::{
        FindDocumentsInput, RunnerReadHooks, RunnerWriteHooks, ServiceContext, ServiceError,
        delete_document_core, find_documents,
    },
};

/// Find all trashed documents via the service layer and permanently delete them.
#[allow(clippy::too_many_arguments)]
fn empty_trash(
    pool: &DbPool,
    runner: &HookRunner,
    def: &CollectionDefinition,
    slug: &str,
    locale_cfg: &LocaleConfig,
    storage: &dyn StorageBackend,
    user_doc: Option<&crate::core::Document>,
    invalidation_transport: Option<SharedInvalidationTransport>,
) -> Result<usize, ServiceError> {
    // Find trashed documents via service (respects access.trash)
    let conn = pool.get().map_err(ServiceError::Internal)?;
    let read_hooks = RunnerReadHooks::new(runner, &conn);

    // System filters (`_deleted_at EXISTS`, `include_deleted`) are injected by
    // the service layer based on `.trash(true)`. We also pass
    // `.include_drafts(true)` because the trash view must show drafts and
    // published rows alike — anything that was soft-deleted, regardless of
    // status, should be eligible for purging.
    let fq = FindQuery::builder().limit(10000).build();

    let read_ctx = ServiceContext::collection(slug, def)
        .pool(pool)
        .conn(&conn)
        .read_hooks(&read_hooks)
        .user(user_doc)
        .build();

    let input = FindDocumentsInput::builder(&fq)
        .hydrate(false)
        .trash(true)
        .include_drafts(true)
        .build();

    let result = find_documents(&read_ctx, &input)?;
    let doc_ids: Vec<String> = result.docs.iter().map(|d| d.id.to_string()).collect();

    drop(conn);

    // Delete each document in a single transaction
    let mut conn = pool.get().map_err(ServiceError::Internal)?;
    let tx = conn
        .transaction_immediate()
        .map_err(ServiceError::Internal)?;

    let wh = RunnerWriteHooks::new(runner).with_conn(&tx);
    let mut hard_def = def.clone();
    hard_def.soft_delete = false;

    let ctx = ServiceContext::collection(slug, &hard_def)
        .conn(&tx)
        .write_hooks(&wh)
        .user(user_doc)
        .invalidation_transport(invalidation_transport)
        .build();

    let mut deleted = 0;
    let mut upload_fields = Vec::new();

    for id in &doc_ids {
        match delete_document_core(&ctx, id, Some(locale_cfg)) {
            Ok(result) => {
                if let Some(fields) = result.upload_doc_fields {
                    upload_fields.push(fields);
                }
                deleted += 1;
            }
            Err(ServiceError::Referenced { .. }) => continue,
            Err(e) => return Err(e),
        }
    }

    tx.commit().map_err(ServiceError::Internal)?;

    for fields in &upload_fields {
        upload::delete_upload_files(storage, fields);
    }

    Ok(deleted)
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
