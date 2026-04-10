//! POST /admin/collections/{slug}/empty-trash — permanently delete all trashed documents.

use anyhow::Context as _;
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
    admin::{AdminState, handlers::shared::check_access_or_forbid},
    config::LocaleConfig,
    core::{CollectionDefinition, auth::AuthUser, upload, upload::StorageBackend},
    db::{
        AccessResult, DbPool,
        query::{self, Filter, FilterClause, FilterOp, FindQuery},
    },
    hooks::HookRunner,
    service::{RunnerWriteHooks, ServiceError, delete_document_core},
};

/// Find all trashed documents and permanently delete them (skipping referenced ones).
fn empty_trash(
    pool: &DbPool,
    runner: &HookRunner,
    def: &CollectionDefinition,
    slug: &str,
    locale_cfg: &LocaleConfig,
    storage: &dyn StorageBackend,
) -> anyhow::Result<usize> {
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let mut fq = FindQuery::new();
    fq.include_deleted = true;
    fq.filters = vec![FilterClause::Single(Filter {
        field: "_deleted_at".to_string(),
        op: FilterOp::Exists,
    })];

    let docs = query::find(&tx, slug, def, &fq, None)?;
    let wh = RunnerWriteHooks::new(runner).with_conn(&tx);
    let mut hard_def = def.clone();
    hard_def.soft_delete = false;

    let mut deleted = 0;
    let mut upload_fields = Vec::new();

    for doc in &docs {
        match delete_document_core(&tx, &wh, slug, &doc.id, &hard_def, None, Some(locale_cfg)) {
            Ok(result) => {
                if let Some(fields) = result.upload_doc_fields {
                    upload_fields.push(fields);
                }
                deleted += 1;
            }
            Err(ServiceError::Referenced { .. }) => continue,
            Err(e) => return Err(e.into_anyhow()),
        }
    }

    tx.commit().context("Commit empty-trash")?;

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

    // Collection-level trash access check — reject early before iterating documents
    let access = check_access_or_forbid(&state, def.access.resolve_trash(), &auth_user, None, None);
    if let Ok(AccessResult::Denied) | Err(_) = access {
        return StatusCode::FORBIDDEN.into_response();
    }

    let pool = state.pool.clone();
    let storage = state.storage.clone();
    let locale_cfg = state.config.locale.clone();
    let runner = state.hook_runner.clone();

    let result = task::spawn_blocking(move || {
        empty_trash(&pool, &runner, &def, &slug, &locale_cfg, &*storage)
    })
    .await;

    match result {
        Ok(Ok(count)) => Json(json!({"ok": true, "count": count})).into_response(),
        Ok(Err(e)) => {
            error!("Empty trash error: {:#}", e);
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
