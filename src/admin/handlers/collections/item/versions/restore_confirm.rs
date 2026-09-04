use axum::{
    Extension,
    extract::{Path, State},
    http::HeaderMap,
    response::Response,
};
use serde_json::json;

use crate::admin::handlers::shared::HxNav;
use crate::{
    admin::{
        AdminState,
        context::{
            BasePageContext, Breadcrumb, CollectionContext, DocumentRef, PageMeta, PageType,
            page::collections::CollectionRestoreConfirmPage,
        },
        handlers::shared::{
            PageRequest, check_access_or_forbid, collection_item_base, extract_editor_locale,
            forbidden, load_version_with_missing_relations, paths, redirect_response, render_page,
            require_collection, server_error,
        },
    },
    core::{
        CollectionDefinition, Document,
        auth::{AuthUser, Claims},
        document::VersionSnapshot,
    },
    db::query::{AccessResult, MissingRelation},
    service::{self, RunnerReadHooks},
};

/// Load the version being restored plus the relations it can no longer
/// resolve.
///
/// Scoped to its own function so the pooled connection and the read hooks
/// borrowing it — neither of which is `Send` — are dropped before the
/// handler awaits the page render.
fn load_restore_data(
    state: &AdminState,
    slug: &str,
    def: &CollectionDefinition,
    version_id: &str,
    user_doc: Option<&Document>,
) -> Result<(VersionSnapshot, Vec<MissingRelation>), &'static str> {
    let Ok(conn) = state.infra.pool.get() else {
        return Err("Database error");
    };

    // `find_version_by_id` (called by `load_version_with_missing_relations`)
    // runs an access check against the collection's `read` access ref, so
    // `ServiceContext.read_hooks` must be wired or it errors out with
    // "read_hooks not set" → 500. The version list handler does the same.
    let read_hooks = RunnerReadHooks::new(&state.infra.hook_runner, &conn, user_doc, None);
    let ctx = service::ServiceContext::collection(slug, def)
        .conn(&conn)
        .read_hooks(&read_hooks)
        .user(user_doc)
        .build();

    load_version_with_missing_relations(&ctx, &conn, &state.infra.registry, version_id, &def.fields)
}

/// GET /`admin/collections/{slug}/{id}/versions/{version_id}/restore` — confirmation page
pub async fn restore_confirm(
    State(state): State<AdminState>,
    hx: HxNav,
    Path((slug, id, version_id)): Path<(String, String, String)>,
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

    match check_access_or_forbid(
        &state,
        def.access.update.as_ref(),
        auth_user.as_ref(),
        Some(&id),
        None,
        "update",
        &slug,
    ) {
        Ok(AccessResult::Denied) => {
            return forbidden(&state, "You don't have permission to update this item");
        }
        Err(resp) => return *resp,
        _ => {}
    }

    let user_doc = auth_user.as_ref().map(|Extension(u)| &u.user_doc);

    let (version, missing) = match load_restore_data(&state, &slug, &def, &version_id, user_doc) {
        Ok(data) => data,
        Err(msg) => return server_error(&state, msg),
    };

    let restore_url = paths::collection_item_version_restore(&slug, &id, &version_id);
    let back_url = paths::collection_item(&slug, &id);

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let claims_ref = claims.as_ref().map(|Extension(c)| c);

    let mut breadcrumbs = collection_item_base(&def, &slug, &id, id.clone());
    breadcrumbs.push(Breadcrumb::current("restore_version"));

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

    render_page(
        &state,
        PageRequest::new(hx, auth_user.as_ref()),
        "collections/restore",
        &ctx,
    )
    .await
}
