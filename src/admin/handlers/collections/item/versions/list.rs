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
            BasePageContext, Breadcrumb, CollectionContext, DocumentRef, PageMeta, PageType,
            PaginationContext, page::collections::CollectionVersionsListPage,
        },
        handlers::shared::{
            Pagination, PaginationParams, extract_editor_locale, get_user_doc, not_found, paths,
            redirect_response, render_page, require_collection, server_error, version_to_json,
        },
    },
    core::{AuthUser, Claims, CollectionDefinition, Document},
    db::query::PaginationResult,
    service::{
        FindByIdInput, ListVersionsInput, RunnerReadHooks, ServiceContext, find_document_by_id,
        list_versions,
    },
};

/// Fetch the document title and paginated version list.
fn fetch_version_data(
    state: &AdminState,
    slug: &str,
    def: &CollectionDefinition,
    id: &str,
    pg: &Pagination,
    user: Option<&Document>,
) -> Result<(String, Vec<Value>, PaginationResult), Box<Response>> {
    let Ok(conn) = state.pool.get() else {
        return Err(Box::new(server_error(state, "Database error")));
    };

    let hooks = RunnerReadHooks::new(&state.hook_runner, &conn, user, None);
    let ctx = ServiceContext::collection(slug, def)
        .conn(&conn)
        .read_hooks(&hooks)
        .user(user)
        .build();

    // Fetch the title through the ACCESS-GATED read — a viewer who cannot read
    // the document must not learn its title/existence via the version page.
    // Read the draft view too so a never-published draft (which still has
    // version snapshots and links here from its edit page) isn't a 404.
    let input = FindByIdInput::builder(id).use_draft(true).build();
    let document = match find_document_by_id(&ctx, &input) {
        Ok(Some(doc)) => doc,
        Ok(None) => {
            return Err(Box::new(not_found(
                state,
                &format!("Document '{id}' not found"),
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
        .map_or_else(|| document.id.to_string(), std::string::ToString::to_string);

    let input = ListVersionsInput::builder(id)
        .limit(Some(pg.per_page))
        .offset(Some(pg.offset))
        .build();

    // Degrade to an empty list when history is not visible (access denial),
    // but log first so a real failure — a backend error or a misconfigured
    // `access.versions` toggle returning a filter table — isn't swallowed.
    let result = list_versions(&ctx, &input)
        .inspect_err(|e| error!("Version list for '{slug}/{id}' failed: {e}"))
        .unwrap_or_default();

    let versions: Vec<Value> = result.docs.iter().map(version_to_json).collect();

    Ok((doc_title, versions, result.pagination))
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
    let def = match require_collection(&state, &slug) {
        Ok(d) => d,
        Err(resp) => return *resp,
    };

    if !def.has_versions() {
        return redirect_response(&paths::collection_item(&slug, &id));
    }

    let pg = params.resolve(&state.config.pagination);
    let user_doc = get_user_doc(auth_user.as_ref());

    let (doc_title, versions, pagination) =
        match fetch_version_data(&state, &slug, &def, &id, &pg, user_doc) {
            Ok(data) => data,
            Err(resp) => return *resp,
        };

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let claims_ref = claims.as_ref().map(|Extension(c)| c);

    let prev_url = paths::collection_item_versions_page(
        &slug,
        &id,
        pg.page.saturating_sub(1).max(1).cast_unsigned(),
    );
    let next_url = paths::collection_item_versions_page(&slug, &id, (pg.page + 1).cast_unsigned());

    let breadcrumbs = vec![
        Breadcrumb::link("collections", paths::COLLECTIONS_ROOT),
        Breadcrumb::link(def.display_name(), paths::collection(&slug)),
        Breadcrumb::link(doc_title.clone(), paths::collection_item(&slug, &id)),
        Breadcrumb::current("version_history"),
    ];

    let base = BasePageContext::for_handler(
        &state,
        claims_ref,
        auth_user.as_ref(),
        PageMeta::new(PageType::CollectionVersions, "version_history_for")
            .with_title_name(doc_title.clone()),
    )
    .with_editor_locale(editor_locale.as_deref(), &state)
    .with_breadcrumbs(breadcrumbs);

    let ctx = CollectionVersionsListPage {
        base,
        collection: CollectionContext::from_def(&def),
        document: DocumentRef::stub(&id),
        pagination: PaginationContext::from_result(&pagination, prev_url, next_url),
        doc_title,
        versions,
        restore_url_prefix: paths::collection_item(&slug, &id),
    };

    render_page(&state, "collections/versions", &ctx)
}
