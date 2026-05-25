use axum::{
    Extension,
    extract::{Path, State},
    http::HeaderMap,
    response::Response,
};
use serde_json::json;

use crate::{
    admin::{
        AdminState,
        context::{
            BasePageContext, Breadcrumb, CollectionContext, DocumentRef, PageMeta, PageType,
            page::collections::CollectionRestoreConfirmPage,
        },
        handlers::shared::{
            check_access_or_forbid, extract_editor_locale, forbidden,
            load_version_with_missing_relations, not_found, paths, redirect_response, render_page,
            server_error,
        },
    },
    core::auth::{AuthUser, Claims},
    db::query::AccessResult,
    service::{self, RunnerReadHooks},
};

/// GET /`admin/collections/{slug}/{id}/versions/{version_id}/restore` — confirmation page
pub async fn restore_confirm(
    State(state): State<AdminState>,
    Path((slug, id, version_id)): Path<(String, String, String)>,
    headers: HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let Some(def) = state.registry.get_collection(&slug).cloned() else {
        return not_found(&state, &format!("Collection '{slug}' not found"));
    };

    if !def.has_versions() {
        return redirect_response(&paths::collection_item(&slug, &id));
    }

    match check_access_or_forbid(
        &state,
        def.access.update.as_deref(),
        auth_user.as_ref(),
        Some(&id),
        None,
    ) {
        Ok(AccessResult::Denied) => {
            return forbidden(&state, "You don't have permission to update this item");
        }
        Err(resp) => return *resp,
        _ => {}
    }

    let Ok(conn) = state.pool.get() else {
        return server_error(&state, "Database error");
    };

    // `find_version_by_id` (called by `load_version_with_missing_relations`)
    // runs an access check against the collection's `read` access ref, so
    // `ServiceContext.read_hooks` must be wired or it errors out with
    // "read_hooks not set" → 500. The version list handler does the same.
    let read_hooks = RunnerReadHooks::new(&state.hook_runner, &conn);
    let user_doc = auth_user.as_ref().map(|Extension(u)| &u.user_doc);
    let version_ctx = service::ServiceContext::collection(&slug, &def)
        .conn(&conn)
        .read_hooks(&read_hooks)
        .user(user_doc)
        .build();

    let (version, missing) = match load_version_with_missing_relations(
        &version_ctx,
        &conn,
        &state.registry,
        &version_id,
        &def.fields,
    ) {
        Ok(data) => data,
        Err(msg) => return server_error(&state, msg),
    };

    let restore_url = paths::collection_item_version_restore(&slug, &id, &version_id);
    let back_url = paths::collection_item(&slug, &id);

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let claims_ref = claims.as_ref().map(|Extension(c)| c);

    let breadcrumbs = vec![
        Breadcrumb::link("collections", paths::COLLECTIONS_ROOT),
        Breadcrumb::link(def.display_name(), paths::collection(&slug)),
        Breadcrumb::link(&id, paths::collection_item(&slug, &id)),
        Breadcrumb::current("restore_version"),
    ];

    let base = BasePageContext::for_handler(
        &state,
        claims_ref,
        auth_user.as_ref(),
        PageMeta::new(PageType::CollectionVersions, "restore_version"),
    )
    .with_editor_locale(editor_locale.as_deref(), &state)
    .with_breadcrumbs(breadcrumbs);

    let ctx = CollectionRestoreConfirmPage {
        base,
        collection: CollectionContext::from_def(&def),
        document: DocumentRef::stub(&id),
        version_number: json!(version.version),
        missing_relations: missing.into_iter().map(|m| json!(m)).collect(),
        restore_url,
        back_url,
    };

    render_page(&state, "collections/restore", &ctx)
}
