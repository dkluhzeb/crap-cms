use axum::{
    Extension,
    extract::{Path, Query, State},
    http::HeaderMap,
    response::Response,
};
use serde_json::{Value, json};

use crate::{
    admin::{
        AdminState,
        context::{Breadcrumb, ContextBuilder, PageType},
        handlers::shared::{
            Pagination, PaginationParams, check_access_or_forbid, extract_editor_locale, forbidden,
            not_found, redirect_response, render_or_error, server_error, version_to_json,
        },
    },
    core::{
        Document,
        auth::{AuthUser, Claims},
    },
    db::{
        BoxedConnection,
        query::{self, AccessResult, helpers::global_table},
    },
};

/// Fetch paginated version list for a global.
fn fetch_version_data(
    conn: &BoxedConnection,
    global_table: &str,
    pg: &Pagination,
) -> (Vec<Value>, i64) {
    let total = query::count_versions(conn, global_table, "default").unwrap_or(0);

    let versions = query::list_versions(
        conn,
        global_table,
        "default",
        Some(pg.per_page),
        Some(pg.offset),
    )
    .unwrap_or_default()
    .into_iter()
    .map(version_to_json)
    .collect();

    (versions, total)
}

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

    match check_access_or_forbid(&state, def.access.read.as_deref(), &auth_user, None, None) {
        Ok(AccessResult::Denied) => {
            return forbidden(&state, "You don't have permission to view this global");
        }
        Err(resp) => return *resp,
        _ => {}
    }

    let conn = match state.pool.get() {
        Ok(c) => c,
        Err(_) => return server_error(&state, "Database error"),
    };

    let global_table = global_table(&slug);
    let pg = params.resolve(&state.config.pagination);
    let (versions, total) = fetch_version_data(&conn, &global_table, &pg);

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let claims_ref = claims.as_ref().map(|Extension(c)| c);
    let data = ContextBuilder::new(&state, claims_ref)
        .locale_from_auth(&auth_user)
        .filter_nav_by_access(&state, &auth_user)
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
            &query::PaginationResult::builder(&[] as &[Document], total, pg.per_page)
                .page(pg.page, pg.offset),
            format!(
                "/admin/globals/{}/versions?page={}",
                slug,
                pg.page.saturating_sub(1).max(1)
            ),
            format!("/admin/globals/{}/versions?page={}", slug, pg.page + 1),
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
