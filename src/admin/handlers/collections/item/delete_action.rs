use axum::{
    Extension,
    body::Bytes,
    extract::{Path, State},
    http::HeaderMap,
    response::IntoResponse,
};

use crate::{
    admin::{AdminState, handlers::collections::shared::delete_action_impl},
    core::auth::AuthUser,
};

/// DELETE /admin/collections/{slug}/{id} — delete an item.
///
/// When the `X-Delete-Dialog` header is present (fetch from the delete dialog),
/// returns JSON `{"ok": true}` on success or `{"error": "..."}` on failure.
/// The `_action` body field controls soft vs hard delete:
/// - `soft_delete` (default): soft-delete if the collection supports it
/// - `hard_delete`: permanently delete even if soft-delete is enabled
pub async fn delete_action(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    headers: HeaderMap,
    auth_user: Option<Extension<AuthUser>>,
    body: Bytes,
) -> impl IntoResponse {
    let from_dialog = headers.contains_key("x-delete-dialog");

    let action = form_urlencoded::parse(&body)
        .find(|(k, _)| k == "_action")
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default();

    let force_hard = action == "hard_delete";

    delete_action_impl(&state, &slug, &id, &auth_user, force_hard, from_dialog).await
}
