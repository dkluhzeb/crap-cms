use axum::{
    extract::{Path, State},
    response::IntoResponse,
    Extension,
};
use crate::admin::AdminState;
use crate::admin::context::{ContextBuilder, PageType, Breadcrumb};
use crate::core::auth::{AuthUser, Claims};
use crate::db::ops;
use crate::db::query::{self, AccessResult};

use crate::admin::handlers::shared::{
    check_access_or_forbid, extract_editor_locale,
    render_or_error, not_found, forbidden,
};

/// GET /admin/collections/{slug}/{id}/delete — delete confirmation page
pub async fn delete_confirm(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> impl IntoResponse {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => return not_found(&state, &format!("Collection '{}' not found", slug)).into_response(),
    };

    // Check delete access
    match check_access_or_forbid(
        &state, def.access.delete.as_deref(), &auth_user, Some(&id), None,
    ) {
        Ok(AccessResult::Denied) => return forbidden(&state, "You don't have permission to delete this item").into_response(),
        Err(resp) => return resp,
        _ => {}
    }

    let title_value = match ops::find_document_by_id(&state.pool, &slug, &def, &id, None) {
        Ok(Some(doc)) => def.title_field()
            .and_then(|f| doc.get_str(f))
            .map(|s| s.to_string()),
        Ok(None) => return not_found(&state, &format!("Document '{}' not found", id)).into_response(),
        Err(e) => {
            // Schema mismatch or other query error — still allow deletion.
            // The DELETE query only needs the ID, not column definitions.
            tracing::warn!("Could not load document for delete confirmation ({}), proceeding anyway: {}", id, e);
            None
        }
    };

    // Scan for back-references
    let back_refs = state.pool.get().ok().map(|conn| {
        query::find_back_references(&conn, &state.registry, &slug, &id, &state.config.locale)
    }).unwrap_or_default();

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let claims_ref = claims.as_ref().map(|Extension(c)| c);
    let data = ContextBuilder::new(&state, claims_ref)
        .locale_from_auth(&auth_user)
        .editor_locale(editor_locale.as_deref(), &state.config.locale)
        .page(PageType::CollectionDelete, format!("Delete {}", def.singular_name()))
        .set("page_title", serde_json::json!(format!("Delete {}", def.singular_name())))
        .collection_def(&def)
        .set("document_id", serde_json::json!(id))
        .set("title_value", serde_json::json!(title_value))
        .set("back_references", serde_json::json!(back_refs))
        .breadcrumbs(vec![
            Breadcrumb::link("Collections", "/admin/collections"),
            Breadcrumb::link(def.display_name(), format!("/admin/collections/{}", slug)),
            Breadcrumb::current(format!("Delete {}", def.singular_name())),
        ])
        .build();

    let data = state.hook_runner.run_before_render(data);

    render_or_error(&state, "collections/delete", &data).into_response()
}
