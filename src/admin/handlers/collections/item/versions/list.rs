use axum::{
    Extension,
    extract::{Path, Query, State},
    http::HeaderMap,
    response::Response,
};
use serde_json::{Value, json};
use tracing::error;

use crate::{
    admin::{
        AdminState,
        context::{Breadcrumb, ContextBuilder, PageType},
        handlers::shared::{
            Pagination, PaginationParams, extract_editor_locale, get_user_doc, not_found,
            redirect_response, render_or_error, server_error, version_to_json,
        },
    },
    core::{
        CollectionDefinition, Document,
        auth::{AuthUser, Claims},
    },
    db::{ops, query, query::LocaleContext},
    service::{RunnerReadHooks, list_versions},
};

/// Fetch the document title and paginated version list.
fn fetch_version_data(
    state: &AdminState,
    slug: &str,
    def: &CollectionDefinition,
    id: &str,
    pg: &Pagination,
    user: Option<&Document>,
) -> Result<(String, Vec<Value>, i64), Box<Response>> {
    let locale_ctx = LocaleContext::from_locale_string(None, &state.config.locale).unwrap_or(None);

    let document = match ops::find_document_by_id(&state.pool, slug, def, id, locale_ctx.as_ref()) {
        Ok(Some(doc)) => doc,
        Ok(None) => {
            return Err(Box::new(not_found(
                state,
                &format!("Document '{}' not found", id),
            )));
        }
        Err(e) => {
            error!("Document versions query error: {}", e);
            return Err(Box::new(server_error(state, "An internal error occurred.")));
        }
    };

    let doc_title = def
        .title_field()
        .and_then(|f| document.get_str(f))
        .map(|s| s.to_string())
        .unwrap_or_else(|| document.id.to_string());

    let conn = match state.pool.get() {
        Ok(c) => c,
        Err(_) => return Err(Box::new(server_error(state, "Database error"))),
    };

    let hooks = RunnerReadHooks::new(&state.hook_runner, &conn);
    let (version_snapshots, total) = list_versions(
        &conn,
        &hooks,
        slug,
        id,
        def.access.read.as_deref(),
        user,
        Some(pg.per_page),
        Some(pg.offset),
    )
    .unwrap_or_default();

    let versions: Vec<Value> = version_snapshots.into_iter().map(version_to_json).collect();

    Ok((doc_title, versions, total))
}

/// GET /admin/collections/{slug}/{id}/versions — dedicated version history page
pub async fn list_versions_page(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    Query(params): Query<PaginationParams>,
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

    let pg = params.resolve(&state.config.pagination);
    let user_doc = get_user_doc(&auth_user);

    let (doc_title, versions, total) =
        match fetch_version_data(&state, &slug, &def, &id, &pg, user_doc) {
            Ok(data) => data,
            Err(resp) => return *resp,
        };

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let claims_ref = claims.as_ref().map(|Extension(c)| c);
    let data = ContextBuilder::new(&state, claims_ref)
        .locale_from_auth(&auth_user)
        .filter_nav_by_access(&state, &auth_user)
        .editor_locale(editor_locale.as_deref(), &state.config.locale)
        .page(PageType::CollectionVersions, "version_history_for")
        .page_title_name(doc_title.clone())
        .collection_def(&def)
        .document_stub(&id)
        .set("doc_title", json!(doc_title))
        .set("versions", json!(versions))
        .set(
            "restore_url_prefix",
            json!(format!("/admin/collections/{}/{}", slug, id)),
        )
        .with_pagination(
            &query::PaginationResult::builder(&[] as &[Document], total, pg.per_page)
                .page(pg.page, pg.offset),
            format!(
                "/admin/collections/{}/{}/versions?page={}",
                slug,
                id,
                pg.page.saturating_sub(1).max(1)
            ),
            format!(
                "/admin/collections/{}/{}/versions?page={}",
                slug,
                id,
                pg.page + 1
            ),
        )
        .breadcrumbs(vec![
            Breadcrumb::link("collections", "/admin/collections"),
            Breadcrumb::link(def.display_name(), format!("/admin/collections/{}", slug)),
            Breadcrumb::link(
                doc_title.clone(),
                format!("/admin/collections/{}/{}", slug, id),
            ),
            Breadcrumb::current("version_history"),
        ])
        .build();

    let data = state.hook_runner.run_before_render(data);

    render_or_error(&state, "collections/versions", &data)
}
