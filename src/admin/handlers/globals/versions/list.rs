use crate::{
    admin::{
        AdminState,
        context::{Breadcrumb, ContextBuilder, PageType},
        handlers::shared::{
            PaginationParams, check_access_or_forbid, extract_editor_locale, forbidden, not_found,
            redirect_response, render_or_error, server_error, version_to_json,
        },
    },
    core::{
        Document,
        auth::{AuthUser, Claims},
    },
    db::query::{self, AccessResult},
};

use axum::{
    Extension,
    extract::{Path, Query, State},
    http::HeaderMap,
    response::Response,
};
use serde_json::{Value, json};

/// GET /admin/globals/{slug}/versions — dedicated version history page
pub async fn list_versions_page(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    Query(params): Query<PaginationParams>,
    headers: HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let def = match state.registry.get_global(&slug) {
        Some(d) => d.clone(),
        None => return not_found(&state, &format!("Global '{}' not found", slug)),
    };

    if !def.has_versions() {
        return redirect_response(&format!("/admin/globals/{}", slug));
    }

    // Check read access
    match check_access_or_forbid(&state, def.access.read.as_deref(), &auth_user, None, None) {
        Ok(AccessResult::Denied) => {
            return forbidden(&state, "You don't have permission to view this global");
        }
        Err(resp) => return *resp,
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
        Err(_) => return server_error(&state, "Database error"),
    };

    let total = query::count_versions(&conn, &global_table, "default").unwrap_or(0);
    let versions: Vec<Value> = query::list_versions(
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
        .page(PageType::GlobalVersions, "version_history_for")
        .page_title_name(def.display_name())
        .global_def(&def)
        .set("versions", json!(versions))
        .set(
            "restore_url_prefix",
            json!(format!("/admin/globals/{}", slug)),
        )
        .with_pagination(
            &query::PaginationResult::builder(&[] as &[Document], total, per_page)
                .page(page, offset),
            format!("/admin/globals/{}/versions?page={}", slug, page - 1),
            format!("/admin/globals/{}/versions?page={}", slug, page + 1),
        )
        .breadcrumbs(vec![
            Breadcrumb::link("dashboard", "/admin"),
            Breadcrumb::link(def.display_name(), format!("/admin/globals/{}", slug)),
            Breadcrumb::current("version_history"),
        ])
        .build();

    let data = state.hook_runner.run_before_render(data);

    render_or_error(&state, "globals/versions", &data)
}
