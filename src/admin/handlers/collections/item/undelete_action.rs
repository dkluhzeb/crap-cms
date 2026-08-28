//! POST /admin/collections/{slug}/{id}/undelete — undelete a soft-deleted document.

use axum::{
    Extension,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use tracing::{error, info};

use std::sync::Arc;

use crate::{
    admin::{
        AdminState,
        handlers::shared::{forbidden, get_user_doc, htmx_redirect, paths},
    },
    core::auth::AuthUser,
    service::{
        ServiceError,
        op::{self, Principal, TargetRef, Undelete, UndeleteArgs},
    },
};

/// POST /admin/collections/{slug}/{id}/undelete — undelete a soft-deleted item
pub async fn undelete_action(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let Some(def) = state.infra.registry.get_collection(&slug).cloned() else {
        return htmx_redirect(paths::COLLECTIONS_ROOT);
    };

    if !def.soft_delete {
        return htmx_redirect(&paths::collection(&slug));
    }

    let result = op::run_blocking::<Undelete>(
        Arc::clone(&state.infra),
        Principal::Resolved {
            user: get_user_doc(auth_user.as_ref()).cloned(),
            ui_locale: auth_user.as_ref().map(|Extension(au)| au.ui_locale.clone()),
        },
        TargetRef::collection(slug.as_str()),
        UndeleteArgs::new(id.as_str()),
    )
    .await;

    match result.map_err(op::CoreError::into_service_error) {
        Ok(_doc) => {
            info!("Undeleted document {} in {}", id, slug);

            htmx_redirect(&paths::collection_trash(&slug))
        }
        Err(ServiceError::AccessDenied(_)) => {
            forbidden(&state, "You don't have permission to undelete this item").into_response()
        }
        Err(e) => {
            error!("Undelete error: {}", e);

            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Undelete failed: {e}"),
            )
                .into_response()
        }
    }
}
