use axum::{
    Extension,
    extract::{Path, State},
    http::HeaderMap,
    response::Response,
};
use serde_json::json;
use tracing::warn;

use crate::{
    admin::{
        AdminState,
        context::{Breadcrumb, ContextBuilder, PageType},
        handlers::shared::{
            check_access_or_forbid, extract_editor_locale, forbidden, lookup_ref_count, not_found,
            render_or_error,
        },
    },
    core::{
        CollectionDefinition,
        auth::{AuthUser, Claims},
    },
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
    let hooks = RunnerReadHooks::new(&state.hook_runner, &conn);

    let ctx = ServiceContext::collection(slug, def)
        .pool(&state.pool)
        .conn(&conn)
        .read_hooks(&hooks)
        .user(user_doc)
        .build();

    let input = FindByIdInput::builder(id).build();

    match find_document_by_id(&ctx, &input) {
        Ok(Some(doc)) => Ok(def
            .title_field()
            .and_then(|f| doc.get_str(f))
            .map(|s| s.to_string())),
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
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => {
            return not_found(&state, &format!("Collection '{}' not found", slug));
        }
    };

    // For soft-delete collections, use trash access (falls back to update).
    // For hard-delete collections, use delete access.
    let access_fn = if def.soft_delete {
        def.access.resolve_trash()
    } else {
        def.access.delete.as_deref()
    };

    match check_access_or_forbid(&state, access_fn, &auth_user, Some(&id), None) {
        Ok(AccessResult::Denied) => {
            return forbidden(&state, "You don't have permission to delete this item");
        }
        Err(resp) => return *resp,
        _ => {}
    }

    let user_doc = auth_user.as_ref().map(|Extension(au)| &au.user_doc);
    let title_value = match fetch_delete_title(&state, &slug, &def, &id, user_doc) {
        Ok(title) => title,
        Err(()) => return not_found(&state, &format!("Document '{}' not found", id)),
    };

    let ref_count = lookup_ref_count(&state.pool, &slug, &id);

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let claims_ref = claims.as_ref().map(|Extension(c)| c);
    let data = ContextBuilder::new(&state, claims_ref)
        .locale_from_auth(&auth_user)
        .filter_nav_by_access(&state, &auth_user)
        .editor_locale(editor_locale.as_deref(), &state.config.locale)
        .page(PageType::CollectionDelete, "delete_name")
        .page_title_name(def.singular_name())
        .collection_def(&def)
        .set("document_id", json!(id))
        .set("title_value", json!(title_value))
        .set("ref_count", json!(ref_count))
        .breadcrumbs(vec![
            Breadcrumb::link("collections", "/admin/collections"),
            Breadcrumb::link(def.display_name(), format!("/admin/collections/{}", slug)),
            Breadcrumb::current("delete_name").with_name(def.singular_name()),
        ])
        .build();

    let data = state.hook_runner.run_before_render(data);

    render_or_error(&state, "collections/delete", &data)
}
