use crate::admin::AdminState;
use crate::admin::context::{Breadcrumb, ContextBuilder, PageType};
use crate::core::auth::{AuthUser, Claims};
use crate::db::query::{AccessResult, LocaleContext};
use crate::db::{ops, query};
use axum::{
    Extension,
    extract::{Path, Query, State},
    response::IntoResponse,
};

use crate::admin::handlers::shared::{
    PaginationParams, check_access_or_forbid, extract_editor_locale, forbidden, not_found,
    redirect_response, render_or_error, server_error, version_to_json,
};

/// GET /admin/collections/{slug}/{id}/versions — dedicated version history page
pub async fn list_versions_page(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    Query(params): Query<PaginationParams>,
    headers: axum::http::HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> impl IntoResponse {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => {
            return not_found(&state, &format!("Collection '{}' not found", slug)).into_response();
        }
    };

    if !def.has_versions() {
        return redirect_response(&format!("/admin/collections/{}/{}", slug, id)).into_response();
    }

    // Check read access
    match check_access_or_forbid(
        &state,
        def.access.read.as_deref(),
        &auth_user,
        Some(&id),
        None,
    ) {
        Ok(AccessResult::Denied) => {
            return forbidden(&state, "You don't have permission to view this item")
                .into_response();
        }
        Err(resp) => return resp,
        _ => {}
    }

    // Build locale context so localized column names resolve correctly
    let locale_ctx = LocaleContext::from_locale_string(None, &state.config.locale);

    // Fetch the document for breadcrumb title
    let document =
        match ops::find_document_by_id(&state.pool, &slug, &def, &id, locale_ctx.as_ref()) {
            Ok(Some(doc)) => doc,
            Ok(None) => {
                return not_found(&state, &format!("Document '{}' not found", id)).into_response();
            }
            Err(e) => {
                tracing::error!("Document versions query error: {}", e);
                return server_error(&state, "An internal error occurred.").into_response();
            }
        };

    let doc_title = def
        .title_field()
        .and_then(|f| document.get_str(f))
        .map(|s| s.to_string())
        .unwrap_or_else(|| document.id.clone());

    let page = params.page.unwrap_or(1).max(1);
    let per_page = params
        .per_page
        .unwrap_or(state.config.pagination.default_limit)
        .min(state.config.pagination.max_limit);
    let offset = (page - 1) * per_page;

    let conn = match state.pool.get() {
        Ok(c) => c,
        Err(_) => return server_error(&state, "Database error").into_response(),
    };

    let total = query::count_versions(&conn, &slug, &id).unwrap_or(0);
    let versions: Vec<serde_json::Value> =
        query::list_versions(&conn, &slug, &id, Some(per_page), Some(offset))
            .unwrap_or_default()
            .into_iter()
            .map(version_to_json)
            .collect();

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let claims_ref = claims.as_ref().map(|Extension(c)| c);
    let data = ContextBuilder::new(&state, claims_ref)
        .locale_from_auth(&auth_user)
        .editor_locale(editor_locale.as_deref(), &state.config.locale)
        .page(
            PageType::CollectionVersions,
            format!("Version History — {}", doc_title),
        )
        .set(
            "page_title",
            serde_json::json!(format!("Version History — {}", doc_title)),
        )
        .collection_def(&def)
        .document_stub(&id)
        .set("doc_title", serde_json::json!(doc_title))
        .set("versions", serde_json::json!(versions))
        .set(
            "restore_url_prefix",
            serde_json::json!(format!("/admin/collections/{}/{}", slug, id)),
        )
        .pagination(
            page,
            per_page,
            total,
            format!(
                "/admin/collections/{}/{}/versions?page={}",
                slug,
                id,
                page - 1
            ),
            format!(
                "/admin/collections/{}/{}/versions?page={}",
                slug,
                id,
                page + 1
            ),
        )
        .breadcrumbs(vec![
            Breadcrumb::link("Collections", "/admin/collections"),
            Breadcrumb::link(def.display_name(), format!("/admin/collections/{}", slug)),
            Breadcrumb::link(
                doc_title.clone(),
                format!("/admin/collections/{}/{}", slug, id),
            ),
            Breadcrumb::current("Version History"),
        ])
        .build();

    let data = state.hook_runner.run_before_render(data);

    render_or_error(&state, "collections/versions", &data).into_response()
}
