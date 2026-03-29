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

use crate::{
    admin::{
        AdminState,
        handlers::shared::{check_access_or_forbid, forbidden},
    },
    core::{auth::AuthUser, upload},
    db::query::{self, AccessResult, Filter, FilterClause, FilterOp, FindQuery},
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

    // Check delete access (empty trash = permanent deletion)
    match def.access.delete.as_deref() {
        None => {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"error": "Permanent deletion not allowed"})),
            )
                .into_response();
        }
        Some(func_ref) => {
            match check_access_or_forbid(&state, Some(func_ref), &auth_user, None, None) {
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
        }
    }

    let pool = state.pool.clone();
    let config_dir = state.config_dir.clone();
    let locale_cfg = state.config.locale.clone();
    let slug_owned = slug.clone();

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

        for doc in &docs {
            // Decrement ref counts before hard delete (CASCADE removes junction rows)
            query::ref_count::before_hard_delete(
                &tx,
                &slug_owned,
                &doc.id,
                &def.fields,
                &locale_cfg,
            )?;

            if def.is_upload_collection() {
                upload::delete_upload_files(&config_dir, &doc.fields);
            }

            query::fts::fts_delete(&tx, &slug_owned, &doc.id)?;
            query::delete(&tx, &slug_owned, &doc.id)?;
        }

        tx.commit().context("Commit empty-trash")?;
        anyhow::Ok(count)
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
