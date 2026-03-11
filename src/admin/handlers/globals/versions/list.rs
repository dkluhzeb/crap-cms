use crate::admin::context::{Breadcrumb, ContextBuilder, PageType};
use crate::admin::AdminState;
use crate::core::auth::{AuthUser, Claims};
use crate::db::query;
use crate::db::query::AccessResult;
use axum::{
    extract::{Path, Query, State},
    response::IntoResponse,
    Extension,
};

use crate::admin::handlers::shared::{
    check_access_or_forbid, extract_editor_locale, forbidden, not_found, redirect_response,
    render_or_error, server_error, version_to_json, PaginationParams,
};

/// GET /admin/globals/{slug}/versions — dedicated version history page
pub async fn list_versions_page(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    Query(params): Query<PaginationParams>,
    headers: axum::http::HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> impl IntoResponse {
    let def = match state.registry.get_global(&slug) {
        Some(d) => d.clone(),
        None => return not_found(&state, &format!("Global '{}' not found", slug)).into_response(),
    };

    if !def.has_versions() {
        return redirect_response(&format!("/admin/globals/{}", slug)).into_response();
    }

    // Check read access
    match check_access_or_forbid(&state, def.access.read.as_deref(), &auth_user, None, None) {
        Ok(AccessResult::Denied) => {
            return forbidden(&state, "You don't have permission to view this global")
                .into_response()
        }
        Err(resp) => return resp,
        _ => {}
    }

    let global_table = format!("_global_{}", slug);
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

    let total = query::count_versions(&conn, &global_table, "default").unwrap_or(0);
    let versions: Vec<serde_json::Value> = query::list_versions(
        &conn,
        &global_table,
        "default",
        Some(per_page),
        Some(offset),
    )
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
            PageType::GlobalVersions,
            format!("Version History — {}", def.display_name()),
        )
        .set(
            "page_title",
            serde_json::json!(format!("Version History — {}", def.display_name())),
        )
        .global_def(&def)
        .set("versions", serde_json::json!(versions))
        .set(
            "restore_url_prefix",
            serde_json::json!(format!("/admin/globals/{}", slug)),
        )
        .pagination(
            page,
            per_page,
            total,
            format!("/admin/globals/{}/versions?page={}", slug, page - 1),
            format!("/admin/globals/{}/versions?page={}", slug, page + 1),
        )
        .breadcrumbs(vec![
            Breadcrumb::link("Dashboard", "/admin"),
            Breadcrumb::link(def.display_name(), format!("/admin/globals/{}", slug)),
            Breadcrumb::current("Version History"),
        ])
        .build();

    let data = state.hook_runner.run_before_render(data);

    render_or_error(&state, "globals/versions", &data).into_response()
}
