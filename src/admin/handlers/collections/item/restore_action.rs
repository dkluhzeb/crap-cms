//! POST /admin/collections/{slug}/{id}/restore — restore a soft-deleted document.

use crate::{
    admin::{
        AdminState,
        handlers::shared::{check_access_or_forbid, forbidden, htmx_redirect},
    },
    core::auth::AuthUser,
    db::query::AccessResult,
    service,
};

use axum::{
    Extension,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use tokio::task;

/// POST /admin/collections/{slug}/{id}/restore — restore a soft-deleted item
pub async fn restore_action(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => return htmx_redirect("/admin/collections"),
    };

    // Check trash access (restore is the inverse of soft-delete)
    let trash_access = def.access.resolve_trash();

    match check_access_or_forbid(&state, trash_access, &auth_user, Some(&id), None) {
        Ok(AccessResult::Denied) => {
            return forbidden(&state, "You don't have permission to restore this item")
                .into_response();
        }
        Err(resp) => return *resp,
        _ => {}
    }

    if !def.soft_delete {
        return htmx_redirect(&format!("/admin/collections/{}", slug));
    }

    let pool = state.pool.clone();
    let slug_owned = slug.clone();
    let id_owned = id.clone();
    let def_owned = def.clone();

    let result = task::spawn_blocking(move || {
        service::restore_document(&pool, &slug_owned, &id_owned, &def_owned)
    })
    .await;

    match result {
        Ok(Ok(_doc)) => {
            tracing::info!("Restored document {} in {}", id, slug);
            htmx_redirect(&format!("/admin/collections/{}?trash=1", slug))
        }
        Ok(Err(e)) => {
            tracing::error!("Restore error: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Restore failed: {}", e),
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!("Restore task error: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Restore failed: {}", e),
            )
                .into_response()
        }
    }
}
