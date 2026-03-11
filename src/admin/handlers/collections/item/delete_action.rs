use crate::admin::AdminState;
use crate::admin::handlers::collections::shared::delete_action_impl;
use crate::core::auth::AuthUser;
use axum::{
    Extension,
    extract::{Path, State},
    response::IntoResponse,
};

/// DELETE /admin/collections/{slug}/{id} — delete an item (no form body)
pub async fn delete_action(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    auth_user: Option<Extension<AuthUser>>,
) -> impl IntoResponse {
    delete_action_impl(&state, &slug, &id, &auth_user)
        .await
        .into_response()
}
