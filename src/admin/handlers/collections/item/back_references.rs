use axum::{
    Extension, Json,
    extract::{Path, State},
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::{
    admin::{AdminState, handlers::shared::check_access_or_forbid},
    core::auth::AuthUser,
    db::query::AccessResult,
    service::{ServiceContext, document_info::find_back_references},
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

    let ctx = ServiceContext::slug_only(&slug).conn(&conn).build();

    let back_refs = match find_back_references(&ctx, &state.registry, &id, &state.config.locale) {
        Ok(refs) => refs,
        Err(_) => {
            return Json(json!({ "error": "Back-reference scan failed" })).into_response();
        }
    };

    Json(json!(back_refs)).into_response()
}
