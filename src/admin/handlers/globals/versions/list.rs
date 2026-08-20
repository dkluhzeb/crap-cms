use axum::{
    Extension,
    extract::{Path, Query, State},
    http::HeaderMap,
    response::Response,
};
use serde_json::Value;
use tracing::error;

use crate::{
    admin::{
        AdminState,
        context::{
            BasePageContext, Breadcrumb, GlobalContext, PageMeta, PageType, PaginationContext,
            page::globals::GlobalVersionsListPage,
        },
        handlers::shared::{
            Pagination, PaginationParams, extract_editor_locale, get_user_doc, paths,
            redirect_response, render_page, require_global, server_error, version_to_json,
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

    // Degrade to an empty list when history is not visible (access denial),
    // but log first so a real failure — a backend error or a misconfigured
    // `access.versions` toggle returning a filter table — isn't swallowed
    // (mirrors the collection version page).
    let result = list_versions(ctx, &input)
        .inspect_err(|e| error!("Global version list for '{}' failed: {e}", ctx.slug))
        .unwrap_or_default();

    let versions = result.docs.iter().map(version_to_json).collect();

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
    let def = match require_global(&state, &slug) {
        Ok(d) => d,
        Err(resp) => return *resp,
    };

    if !def.has_versions() {
        return redirect_response(&paths::global(&slug));
    }

    let Ok(conn) = state.pool.get() else {
        return server_error(&state, "Database error");
    };

    let user_doc = get_user_doc(auth_user.as_ref());
    let pg = params.resolve(&state.config.pagination);
    let hooks = RunnerReadHooks::new(&state.hook_runner, &conn, user_doc, None);

    let ctx = ServiceContext::global(&slug, &def)
        .conn(&conn)
        .read_hooks(&hooks)
        .user(user_doc)
        .build();

    let (versions, pagination) = fetch_version_data(&ctx, &pg);

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let claims_ref = claims.as_ref().map(|Extension(c)| c);

    let prev_url =
        paths::global_versions_page(&slug, pg.page.saturating_sub(1).max(1).cast_unsigned());
    let next_url = paths::global_versions_page(&slug, (pg.page + 1).cast_unsigned());

    let breadcrumbs = vec![
        Breadcrumb::link("dashboard", paths::DASHBOARD),
        Breadcrumb::link(def.display_name(), paths::global(&slug)),
        Breadcrumb::current("version_history"),
    ];

    let base = BasePageContext::for_handler(
        &state,
        claims_ref,
        auth_user.as_ref(),
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
