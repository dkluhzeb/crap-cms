use axum::{
    Extension, Json,
    extract::{Path, State},
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::{
    admin::{AdminState, handlers::shared::check_access_or_forbid},
    core::auth::AuthUser,
    db::query::{self, AccessResult},
};

/// GET /admin/collections/{slug}/{id}/back-references — lazy-load detailed back-references
pub async fn back_references(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => return Json(json!({ "error": "Collection not found" })).into_response(),
    };

    match check_access_or_forbid(
        &state,
        def.access.read.as_deref(),
        &auth_user,
        Some(&id),
        None,
    ) {
        Ok(AccessResult::Denied) => {
            return Json(json!({ "error": "Access denied" })).into_response();
        }
        Err(_) => return Json(json!({ "error": "Access denied" })).into_response(),
        _ => {}
    }

    let conn = match state.pool.get() {
        Ok(c) => c,
        Err(_) => return Json(json!({ "error": "DB connection error" })).into_response(),
    };

    let back_refs =
        query::find_back_references(&conn, &state.registry, &slug, &id, &state.config.locale);

    Json(json!(back_refs)).into_response()
}
