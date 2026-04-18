//! POST /admin/collections/{slug}/{id}/undelete — undelete a soft-deleted document.

use axum::{
    Extension,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use tokio::task;
use tracing::{error, info};

use crate::{
    admin::{
        AdminState,
        handlers::shared::{forbidden, htmx_redirect},
    },
    core::auth::AuthUser,
    service::{self, ServiceContext, ServiceError},
};

/// POST /admin/collections/{slug}/{id}/undelete — undelete a soft-deleted item
pub async fn undelete_action(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => return htmx_redirect("/admin/collections"),
    };

    if !def.soft_delete {
        return htmx_redirect(&format!("/admin/collections/{}", slug));
    }

    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let slug_owned = slug.clone();
    let id_owned = id.clone();
    let def_owned = def.clone();
    let user_doc = crate::admin::handlers::shared::get_user_doc(&auth_user).cloned();
    let event_transport = state.event_transport.clone();

    let result = task::spawn_blocking(move || {
        let ctx = ServiceContext::collection(&slug_owned, &def_owned)
            .pool(&pool)
            .runner(&runner)
            .user(user_doc.as_ref())
            .event_transport(event_transport)
            .build();

        service::undelete_document(&ctx, &id_owned)
    })
    .await;

    match result {
        Ok(Ok(_doc)) => {
            info!("Undeleted document {} in {}", id, slug);

            htmx_redirect(&format!("/admin/collections/{}?trash=1", slug))
        }
        Ok(Err(ServiceError::AccessDenied(_))) => {
            forbidden(&state, "You don't have permission to undelete this item").into_response()
        }
        Ok(Err(e)) => {
            error!("Undelete error: {}", e);

            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Undelete failed: {}", e),
            )
                .into_response()
        }
        Err(e) => {
            error!("Undelete task error: {}", e);

            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Undelete failed: {}", e),
            )
                .into_response()
        }
    }
}
