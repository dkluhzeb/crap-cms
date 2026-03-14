use crate::{
    admin::{
        AdminState,
        context::{Breadcrumb, ContextBuilder, PageType},
        handlers::shared::{
            check_access_or_forbid, extract_editor_locale, forbidden, not_found, redirect_response,
            render_or_error, server_error,
        },
    },
    core::auth::{AuthUser, Claims},
    db::query::{AccessResult, find_missing_relations, find_version_by_id},
};

use axum::{
    Extension,
    extract::{Path, State},
    http::HeaderMap,
    response::Response,
};
use serde_json::json;

/// GET /admin/collections/{slug}/{id}/versions/{version_id}/restore — confirmation page
pub async fn restore_confirm(
    State(state): State<AdminState>,
    Path((slug, id, version_id)): Path<(String, String, String)>,
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

    if !def.has_versions() {
        return redirect_response(&format!("/admin/collections/{}/{}", slug, id));
    }

    match check_access_or_forbid(
        &state,
        def.access.update.as_deref(),
        &auth_user,
        Some(&id),
        None,
    ) {
        Ok(AccessResult::Denied) => {
            return forbidden(&state, "You don't have permission to update this item");
        }
        Err(resp) => return *resp,
        _ => {}
    }

    let conn = match state.pool.get() {
        Ok(c) => c,
        Err(_) => return server_error(&state, "Database error"),
    };

    let version = match find_version_by_id(&conn, &slug, &version_id) {
        Ok(Some(v)) => v,
        Ok(None) => return not_found(&state, "Version not found"),
        Err(e) => {
            tracing::error!("Find version error: {}", e);
            return server_error(&state, "Database error");
        }
    };

    let missing = find_missing_relations(&conn, &state.registry, &version.snapshot, &def.fields);

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
        .page(PageType::CollectionVersions, "restore_version")
        .collection_def(&def)
        .document_stub(&id)
        .set("version_number", json!(version.version))
        .set("missing_relations", json!(missing))
        .set("restore_url", json!(restore_url))
        .set("back_url", json!(back_url))
        .breadcrumbs(vec![
            Breadcrumb::link("collections", "/admin/collections"),
            Breadcrumb::link(def.display_name(), format!("/admin/collections/{}", slug)),
            Breadcrumb::link(&id, format!("/admin/collections/{}/{}", slug, id)),
            Breadcrumb::current("restore_version"),
        ])
        .build();

    let data = state.hook_runner.run_before_render(data);

    render_or_error(&state, "collections/restore", &data)
}
