//! POST /admin/collections/{slug}/empty-trash — permanently delete all trashed documents.

use std::collections::HashMap;

use anyhow::Context as _;
use axum::{
    Extension,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use serde_json::{Value, json};
use tokio::task;
use tracing::{debug, error};

use crate::{
    admin::{
        AdminState,
        handlers::shared::{check_access_or_forbid, forbidden},
    },
    config::LocaleConfig,
    core::{CollectionDefinition, Document, auth::AuthUser, upload, upload::StorageBackend},
    db::{
        BoxedTransaction, DbConnection, DbPool,
        query::{self, AccessResult, Filter, FilterClause, FilterOp, FindQuery},
    },
    hooks::{HookContext, HookEvent, HookRunner},
};

/// Hard-delete a single document: run hooks, clean up refs/uploads/FTS, delete row.
///
/// Returns `true` if deleted, `false` if skipped (still referenced).
fn hard_delete_one(
    tx: &BoxedTransaction,
    runner: &HookRunner,
    def: &CollectionDefinition,
    slug: &str,
    doc: &Document,
    locale_cfg: &LocaleConfig,
    storage: &dyn StorageBackend,
) -> anyhow::Result<bool> {
    let ref_count = query::ref_count::get_ref_count(tx, slug, &doc.id)?.unwrap_or(0);

    if ref_count > 0 {
        debug!(
            "Skipping permanent delete of {}/{}: referenced by {} document(s)",
            slug, doc.id, ref_count
        );
        return Ok(false);
    }

    let hook_data: HashMap<String, Value> =
        [("id".into(), Value::String(doc.id.to_string()))].into();

    let hook_ctx = HookContext::builder(slug, "delete").data(hook_data).build();

    runner.run_hooks_with_conn(&def.hooks, HookEvent::BeforeDelete, hook_ctx, tx)?;

    query::ref_count::before_hard_delete(tx, slug, &doc.id, &def.fields, locale_cfg)?;

    if def.is_upload_collection() {
        upload::delete_upload_files(storage, &doc.fields);
        let _ = query::images::delete_entries_for_document(tx, slug, &doc.id);
    }

    if tx.supports_fts() {
        query::fts::fts_delete(tx, slug, &doc.id)?;
    }

    query::delete(tx, slug, &doc.id)?;

    let after_data: HashMap<String, Value> =
        [("id".into(), Value::String(doc.id.to_string()))].into();

    let after_ctx = HookContext::builder(slug, "delete")
        .data(after_data)
        .build();

    let _ = runner.run_hooks(&def.hooks, HookEvent::AfterDelete, after_ctx);

    Ok(true)
}

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
    let mut deleted = 0;

    for doc in &docs {
        if hard_delete_one(&tx, runner, def, slug, doc, locale_cfg, storage)? {
            deleted += 1;
        }
    }

    tx.commit().context("Commit empty-trash")?;

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

    match check_access_or_forbid(&state, def.access.delete.as_deref(), &auth_user, None, None) {
        Ok(AccessResult::Denied) => {
            return forbidden(
                &state,
                "You don't have permission to permanently delete items",
            )
            .into_response();
        }
        Err(resp) => return *resp,
        _ => {}
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
