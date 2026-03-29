use crate::{
    admin::{
        AdminState,
        context::{Breadcrumb, ContextBuilder, PageType},
        handlers::shared::{
            check_access_or_forbid, extract_editor_locale, forbidden, not_found, render_or_error,
        },
    },
    core::auth::{AuthUser, Claims},
    db::{
        ops::find_document_by_id,
        query::{self, AccessResult},
    },
};

use axum::{
    Extension, Json,
    extract::{Path, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use serde_json::json;
use tracing::warn;

/// GET /admin/collections/{slug}/{id}/delete — delete confirmation page
pub async fn delete_confirm(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    headers: HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => {
            return not_found(&state, &format!("Collection '{}' not found", slug));
        }
    };

    // Check delete access
    match check_access_or_forbid(
        &state,
        def.access.delete.as_deref(),
        &auth_user,
        Some(&id),
        None,
    ) {
        Ok(AccessResult::Denied) => {
            return forbidden(&state, "You don't have permission to delete this item");
        }
        Err(resp) => return *resp,
        _ => {}
    }

    let title_value = match find_document_by_id(&state.pool, &slug, &def, &id, None) {
        Ok(Some(doc)) => def
            .title_field()
            .and_then(|f| doc.get_str(f))
            .map(|s| s.to_string()),
        Ok(None) => {
            return not_found(&state, &format!("Document '{}' not found", id));
        }
        Err(e) => {
            // Schema mismatch or other query error — still allow deletion.
            // The DELETE query only needs the ID, not column definitions.
            warn!(
                "Could not load document for delete confirmation ({}), proceeding anyway: {}",
                id, e
            );

            None
        }
    };

    // Fast O(1) ref count check instead of full back-reference scan
    let ref_count = state
        .pool
        .get()
        .ok()
        .and_then(|conn| query::ref_count::get_ref_count(&conn, &slug, &id).ok())
        .unwrap_or(0);

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let claims_ref = claims.as_ref().map(|Extension(c)| c);
    let data = ContextBuilder::new(&state, claims_ref)
        .locale_from_auth(&auth_user)
        .filter_nav_by_access(&state, &auth_user)
        .editor_locale(editor_locale.as_deref(), &state.config.locale)
        .page(PageType::CollectionDelete, "delete_name")
        .page_title_name(def.singular_name())
        .collection_def(&def)
        .set("document_id", json!(id))
        .set("title_value", json!(title_value))
        .set("ref_count", json!(ref_count))
        .breadcrumbs(vec![
            Breadcrumb::link("collections", "/admin/collections"),
            Breadcrumb::link(def.display_name(), format!("/admin/collections/{}", slug)),
            Breadcrumb::current("delete_name").with_name(def.singular_name()),
        ])
        .build();

    let data = state.hook_runner.run_before_render(data);

    render_or_error(&state, "collections/delete", &data)
}

/// GET /admin/collections/{slug}/{id}/back-references — lazy-load detailed back-references
pub async fn back_references(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
) -> Response {
    let conn = match state.pool.get() {
        Ok(c) => c,
        Err(_) => return Json(json!({ "error": "DB connection error" })).into_response(),
    };

    let back_refs =
        query::find_back_references(&conn, &state.registry, &slug, &id, &state.config.locale);

    Json(json!(back_refs)).into_response()
}
