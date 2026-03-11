use crate::admin::context::{Breadcrumb, ContextBuilder, PageType};
use crate::admin::AdminState;
use crate::core::auth::{AuthUser, Claims};
use crate::db::query::{self, AccessResult};
use axum::{
    extract::{Path, State},
    response::IntoResponse,
    Extension,
};

use crate::admin::handlers::shared::{
    check_access_or_forbid, extract_editor_locale, forbidden, not_found, redirect_response,
    render_or_error, server_error,
};

/// GET /admin/collections/{slug}/{id}/versions/{version_id}/restore — confirmation page
pub async fn restore_confirm(
    State(state): State<AdminState>,
    Path((slug, id, version_id)): Path<(String, String, String)>,
    headers: axum::http::HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> impl IntoResponse {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => {
            return not_found(&state, &format!("Collection '{}' not found", slug)).into_response()
        }
    };

    if !def.has_versions() {
        return redirect_response(&format!("/admin/collections/{}/{}", slug, id)).into_response();
    }

    match check_access_or_forbid(
        &state,
        def.access.update.as_deref(),
        &auth_user,
        Some(&id),
        None,
    ) {
        Ok(AccessResult::Denied) => {
            return forbidden(&state, "You don't have permission to update this item")
                .into_response()
        }
        Err(resp) => return resp,
        _ => {}
    }

    let conn = match state.pool.get() {
        Ok(c) => c,
        Err(_) => return server_error(&state, "Database error").into_response(),
    };

    let version = match query::find_version_by_id(&conn, &slug, &version_id) {
        Ok(Some(v)) => v,
        Ok(None) => return not_found(&state, "Version not found").into_response(),
        Err(e) => {
            tracing::error!("Find version error: {}", e);
            return server_error(&state, "Database error").into_response();
        }
    };

    let missing =
        query::find_missing_relations(&conn, &state.registry, &version.snapshot, &def.fields);

    let restore_url = format!(
        "/admin/collections/{}/{}/versions/{}/restore",
        slug, id, version_id
    );
    let back_url = format!("/admin/collections/{}/{}", slug, id);

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let claims_ref = claims.as_ref().map(|Extension(c)| c);
    let data = ContextBuilder::new(&state, claims_ref)
        .locale_from_auth(&auth_user)
        .editor_locale(editor_locale.as_deref(), &state.config.locale)
        .page(PageType::CollectionVersions, "Restore Version".to_string())
        .collection_def(&def)
        .document_stub(&id)
        .set("version_number", serde_json::json!(version.version))
        .set("missing_relations", serde_json::json!(missing))
        .set("restore_url", serde_json::json!(restore_url))
        .set("back_url", serde_json::json!(back_url))
        .breadcrumbs(vec![
            Breadcrumb::link("Collections", "/admin/collections"),
            Breadcrumb::link(def.display_name(), format!("/admin/collections/{}", slug)),
            Breadcrumb::link(&id, format!("/admin/collections/{}/{}", slug, id)),
            Breadcrumb::current("Restore Version"),
        ])
        .build();

    let data = state.hook_runner.run_before_render(data);
    render_or_error(&state, "collections/restore", &data).into_response()
}
