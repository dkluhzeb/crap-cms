use axum::{
    Extension,
    extract::{Path, Query, State},
    http::HeaderMap,
    response::Response,
};
use serde_json::Value;

use crate::{
    admin::{
        AdminState,
        context::{
            BasePageContext, Breadcrumb, GlobalContext, PageMeta, PageType, PaginationContext,
            page::globals::GlobalVersionsListPage,
        },
        handlers::shared::{
            Pagination, PaginationParams, extract_editor_locale, get_user_doc, not_found, paths,
            redirect_response, render_page, server_error, version_to_json,
        },
    },
    core::auth::{AuthUser, Claims},
    db::query::PaginationResult,
    service::{ListVersionsInput, RunnerReadHooks, ServiceContext, list_versions},
};

/// Fetch paginated version list for a global.
fn fetch_version_data(ctx: &ServiceContext, pg: &Pagination) -> (Vec<Value>, PaginationResult) {
    let input = ListVersionsInput::builder("default")
        .limit(Some(pg.per_page))
        .offset(Some(pg.offset))
        .build();

    let result = list_versions(ctx, &input).unwrap_or_default();

    let versions = result.docs.into_iter().map(version_to_json).collect();

    (versions, result.pagination)
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
        return redirect_response(&paths::global(&slug));
    }

    let conn = match state.pool.get() {
        Ok(c) => c,
        Err(_) => return server_error(&state, "Database error"),
    };

    let user_doc = get_user_doc(&auth_user);
    let pg = params.resolve(&state.config.pagination);
    let hooks = RunnerReadHooks::new(&state.hook_runner, &conn);

    let ctx = ServiceContext::global(&slug, &def)
        .conn(&conn)
        .read_hooks(&hooks)
        .user(user_doc)
        .build();

    let (versions, pagination) = fetch_version_data(&ctx, &pg);

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let claims_ref = claims.as_ref().map(|Extension(c)| c);

    let prev_url = paths::global_versions_page(&slug, pg.page.saturating_sub(1).max(1) as u64);
    let next_url = paths::global_versions_page(&slug, (pg.page + 1) as u64);

    let breadcrumbs = vec![
        Breadcrumb::link("dashboard", "/admin"),
        Breadcrumb::link(def.display_name(), paths::global(&slug)),
        Breadcrumb::current("version_history"),
    ];

    let base = BasePageContext::for_handler(
        &state,
        claims_ref,
        &auth_user,
        PageMeta::new(PageType::GlobalVersions, "version_history_for")
            .with_title_name(def.display_name()),
    )
    .with_editor_locale(editor_locale.as_deref(), &state)
    .with_breadcrumbs(breadcrumbs);

    let ctx = GlobalVersionsListPage {
        base,
        global: GlobalContext::from_def(&def),
        pagination: PaginationContext::from_result(&pagination, prev_url, next_url),
        versions,
        restore_url_prefix: paths::global(&slug),
    };

    render_page(&state, "globals/versions", &ctx)
}
