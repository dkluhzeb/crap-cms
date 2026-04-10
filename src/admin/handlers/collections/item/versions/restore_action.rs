use axum::{
    Extension,
    extract::{Path, State},
    response::Response,
};
use tokio::task;
use tracing::error;

use crate::{
    admin::{
        AdminState,
        handlers::shared::{get_user_doc, htmx_redirect, redirect_response},
    },
    core::auth::AuthUser,
    service::restore_collection_version,
};

/// POST /admin/collections/{slug}/{id}/versions/{version_id}/restore — restore a version
pub async fn restore_version(
    State(state): State<AdminState>,
    Path((slug, id, version_id)): Path<(String, String, String)>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => return redirect_response("/admin/collections"),
    };

    if !def.has_versions() {
        return redirect_response(&format!("/admin/collections/{}/{}", slug, id));
    }

    let redirect = format!("/admin/collections/{}/{}", slug, id);
    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let locale_config = state.config.locale.clone();
    let user_doc = get_user_doc(&auth_user).cloned();

    let result = task::spawn_blocking(move || {
        restore_collection_version(
            &pool,
            &runner,
            &slug,
            &def,
            &id,
            &version_id,
            &locale_config,
            user_doc.as_ref(),
            false,
        )
    })
    .await;

    match result {
        Ok(Ok(_)) => htmx_redirect(&redirect),
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
