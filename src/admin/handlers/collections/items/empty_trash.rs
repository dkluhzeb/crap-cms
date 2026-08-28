//! POST /admin/collections/{slug}/empty-trash — permanently delete all trashed documents.

use std::sync::Arc;

use axum::{
    Extension,
    extract::{Path, State},
    response::{IntoResponse, Json, Response},
};
use serde_json::json;
use tracing::error;

use crate::{
    admin::{
        AdminState,
        handlers::shared::{
            json_bad_request, json_forbidden, json_server_error, require_collection_json,
        },
    },
    core::AuthUser,
    service::{
        ServiceError,
        op::{self, DeleteMany, DeleteManyArgs, Principal, TargetRef},
    },
};

/// POST /admin/collections/{slug}/empty-trash
///
/// Codec over the shared [`DeleteMany`] operation body: `trash = true` carries
/// the whole purge semantics (hard-delete-adjusted definition, `_deleted_at`
/// restriction, `include_deleted`, post-commit upload-file cleanup) — this
/// handler only gates the capability and shapes the JSON response.
#[cfg(not(tarpaulin_include))]
pub async fn empty_trash_action(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let def = match require_collection_json(&state, &slug) {
        Ok(d) => d,
        Err(resp) => return *resp,
    };

    if !def.soft_delete {
        return json_bad_request("Collection does not support soft delete");
    }

    let user_doc = auth_user.as_ref().map(|Extension(au)| au.user_doc.clone());

    let args = DeleteManyArgs::builder(Vec::new())
        .trash(true)
        .max_documents(state.config.server.bulk_max_documents)
        // Bulk trash purge is quiet — no per-document live-update events.
        .events(false)
        .build();

    let result = op::run_blocking::<DeleteMany>(
        Arc::clone(&state.infra),
        Principal::Resolved {
            user: user_doc,
            ui_locale: auth_user.as_ref().map(|Extension(au)| au.ui_locale.clone()),
        },
        TargetRef::collection(slug),
        args,
    )
    .await;

    match result.map_err(op::CoreError::into_service_error) {
        Ok(res) => {
            let count = usize::try_from(res.hard_deleted.max(0)).unwrap_or(0);
            Json(json!({"ok": true, "count": count})).into_response()
        }
        Err(ServiceError::AccessDenied(_)) => {
            json_forbidden("You don't have permission to empty the trash")
        }
        Err(e) => {
            error!("Empty trash error: {}", e);
            json_server_error("Failed to empty trash")
        }
    }
}
