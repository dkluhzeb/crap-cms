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
    service::{ServiceContext, restore_global_version},
};

/// POST /admin/globals/{slug}/versions/{version_id}/restore
pub async fn restore_version(
    State(state): State<AdminState>,
    Path((slug, version_id)): Path<(String, String)>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let def = match state.registry.get_global(&slug) {
        Some(d) => d.clone(),
        None => return redirect_response("/admin"),
    };

    if !def.has_versions() {
        return redirect_response(&format!("/admin/globals/{}", slug));
    }

    let redirect = format!("/admin/globals/{}", slug);
    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let locale_config = state.config.locale.clone();
    let user_doc = get_user_doc(&auth_user).cloned();
    let event_transport = state.event_transport.clone();
    let cache = state.cache.clone();

    let result = task::spawn_blocking(move || {
        let ctx = ServiceContext::global(&slug, &def)
            .pool(&pool)
            .runner(&runner)
            .user(user_doc.as_ref())
            .event_transport(event_transport)
            .cache(cache)
            .build();

        restore_global_version(&ctx, &version_id, &locale_config)
    })
    .await;

    match result {
        Ok(Ok(_)) => htmx_redirect(&redirect),
        Ok(Err(e)) => {
            error!("Restore global version error: {}", e);
            htmx_redirect(&redirect)
        }
        Err(e) => {
            error!("Restore global version task error: {}", e);
            htmx_redirect(&redirect)
        }
    }
}
