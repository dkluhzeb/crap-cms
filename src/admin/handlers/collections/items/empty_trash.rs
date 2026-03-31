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

use std::collections::HashMap;

use crate::{
    admin::{
        AdminState,
        handlers::shared::{check_access_or_forbid, forbidden},
    },
    core::{auth::AuthUser, upload},
    db::{
        DbConnection,
        query::{self, AccessResult, Filter, FilterClause, FilterOp, FindQuery},
    },
    hooks::{HookContext, HookEvent},
};

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

    // Check delete access (empty trash = permanent deletion).
    // Uses check_access_or_forbid which respects default_deny config.
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
    let slug_owned = slug.clone();
    let runner = state.hook_runner.clone();

    let result = task::spawn_blocking(move || {
        let mut conn = pool.get().context("DB connection")?;
        let tx = conn.transaction_immediate().context("Start transaction")?;

        // Find all soft-deleted documents
        let mut fq = FindQuery::new();
        fq.include_deleted = true;
        fq.filters = vec![FilterClause::Single(Filter {
            field: "_deleted_at".to_string(),
            op: FilterOp::Exists,
        })];

        let docs = query::find(&tx, &slug_owned, &def, &fq, None)?;
        let count = docs.len();

        let mut skipped = 0usize;

        for doc in &docs {
            // Skip documents that are still referenced — protect referential integrity
            let ref_count =
                query::ref_count::get_ref_count(&tx, &slug_owned, &doc.id)?.unwrap_or(0);
            if ref_count > 0 {
                tracing::debug!(
                    "Skipping permanent delete of {}/{}: referenced by {} document(s)",
                    slug_owned,
                    doc.id,
                    ref_count
                );
                skipped += 1;
                continue;
            }

            // Run BeforeDelete hook
            let hook_data: HashMap<String, serde_json::Value> =
                [("id".into(), serde_json::Value::String(doc.id.to_string()))].into();
            let hook_ctx = HookContext::builder(&slug_owned, "delete")
                .data(hook_data)
                .build();
            runner.run_hooks_with_conn(&def.hooks, HookEvent::BeforeDelete, hook_ctx, &tx)?;

            // Decrement ref counts before hard delete (CASCADE removes junction rows)
            query::ref_count::before_hard_delete(
                &tx,
                &slug_owned,
                &doc.id,
                &def.fields,
                &locale_cfg,
            )?;

            if def.is_upload_collection() {
                upload::delete_upload_files(&*storage, &doc.fields);
                let _ = query::images::delete_entries_for_document(&tx, &slug_owned, &doc.id);
            }

            if tx.supports_fts() {
                query::fts::fts_delete(&tx, &slug_owned, &doc.id)?;
            }
            query::delete(&tx, &slug_owned, &doc.id)?;

            // Run AfterDelete hook (fire-and-forget, no CRUD access)
            let after_data: HashMap<String, serde_json::Value> =
                [("id".into(), serde_json::Value::String(doc.id.to_string()))].into();
            let after_ctx = HookContext::builder(&slug_owned, "delete")
                .data(after_data)
                .build();
            let _ = runner.run_hooks(&def.hooks, HookEvent::AfterDelete, after_ctx);
        }

        tx.commit().context("Commit empty-trash")?;
        anyhow::Ok(count - skipped)
    })
    .await;

    match result {
        Ok(Ok(count)) => Json(json!({"ok": true, "count": count})).into_response(),
        Ok(Err(e)) => {
            tracing::error!("Empty trash error: {:#}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Failed to empty trash"})),
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!("Empty trash task error: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Internal error"})),
            )
                .into_response()
        }
    }
}
