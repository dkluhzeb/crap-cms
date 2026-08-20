use axum::{
    Extension,
    extract::{Path, State},
    http::HeaderMap,
    response::Response,
};
use tracing::warn;

use crate::{
    admin::{
        AdminState,
        context::{
            BasePageContext, Breadcrumb, CollectionContext, PageMeta, PageType,
            page::collections::CollectionDeleteConfirmPage,
        },
        handlers::shared::{
            check_access_or_forbid, extract_editor_locale, forbidden, lookup_ref_count, not_found,
            paths, render_page, require_collection,
        },
    },
    core::{AuthUser, Claims, CollectionDefinition},
    db::query::AccessResult,
    service::{FindByIdInput, RunnerReadHooks, ServiceContext, find_document_by_id},
};

/// Fetch the document title for display in the delete confirmation page.
fn fetch_delete_title(
    state: &AdminState,
    slug: &str,
    def: &CollectionDefinition,
    id: &str,
    user_doc: Option<&crate::core::Document>,
) -> Result<Option<String>, ()> {
    let conn = state.pool.get().map_err(|_| ())?;
    let hooks = RunnerReadHooks::new(&state.hook_runner, &conn, user_doc, None);

    let ctx = ServiceContext::collection(slug, def)
        .pool(&state.pool)
        .conn(&conn)
        .read_hooks(&hooks)
        .user(user_doc)
        .build();

    // Read the draft view too: a draft-only document (never published) shows in
    // the list (which includes drafts) and must be deletable through this confirm
    // page. Without `use_draft`, the published-only read returns None → 404. The
    // ctx is user-scoped, so read-only users still downgrade safely.
    let input = FindByIdInput::builder(id).use_draft(true).build();

    match find_document_by_id(&ctx, &input) {
        Ok(Some(doc)) => Ok(def
            .title_field()
            .and_then(|f| doc.get_str(f))
            .map(std::string::ToString::to_string)),
        Ok(None) => Err(()),
        Err(e) => {
            warn!(
                "Could not load document for delete confirmation ({}), proceeding anyway: {}",
                id, e
            );
            Ok(None)
        }
    }
}

/// GET /admin/collections/{slug}/{id}/delete — delete confirmation page
pub async fn delete_confirm(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    headers: HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let def = match require_collection(&state, &slug) {
        Ok(d) => d,
        Err(resp) => return *resp,
    };

    // For soft-delete collections, use trash access (falls back to update).
    // For hard-delete collections, use delete access.
    let access_fn = if def.soft_delete {
        def.access.resolve_trash()
    } else {
        def.access.delete.as_ref()
    };

    // A soft delete is a "trash" operation (gated by the trash access fn);
    // a hard delete is "delete". Matches the actual delete action + the grid.
    let (op, verb) = if def.soft_delete {
        ("trash", "trash")
    } else {
        ("delete", "delete")
    };

    match check_access_or_forbid(
        &state,
        access_fn,
        auth_user.as_ref(),
        Some(&id),
        None,
        op,
        &slug,
    ) {
        Ok(AccessResult::Denied) => {
            return forbidden(
                &state,
                &format!("You don't have permission to {verb} this item"),
            );
        }
        Err(resp) => return *resp,
        _ => {}
    }

    let user_doc = auth_user.as_ref().map(|Extension(au)| &au.user_doc);
    let Ok(title_value) = fetch_delete_title(&state, &slug, &def, &id, user_doc) else {
        return not_found(&state, &format!("Document '{id}' not found"));
    };

    let ref_count = lookup_ref_count(&state.pool, &slug, &id);

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let claims_ref = claims.as_ref().map(|Extension(c)| c);

    let breadcrumbs = vec![
        Breadcrumb::link("collections", paths::COLLECTIONS_ROOT),
        Breadcrumb::link(def.display_name(), paths::collection(&slug)),
        Breadcrumb::current("delete_name").with_name(def.singular_name()),
    ];

    let base = BasePageContext::for_handler(
        &state,
        claims_ref,
        auth_user.as_ref(),
        PageMeta::new(PageType::CollectionDelete, "delete_name")
            .with_title_name(def.singular_name()),
    )
    .with_editor_locale(editor_locale.as_deref(), &state)
    .with_breadcrumbs(breadcrumbs);

    let ctx = CollectionDeleteConfirmPage {
        base,
        collection: CollectionContext::from_def(&def),
        document_id: id,
        title_value,
        ref_count,
    };

    render_page(&state, "collections/delete", &ctx)
}
