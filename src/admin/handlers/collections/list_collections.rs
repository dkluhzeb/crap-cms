use axum::{Extension, extract::State, http::HeaderMap, response::Response};

use crate::{
    admin::{
        AdminState,
        context::{
            BasePageContext, PageMeta, PageType,
            page::collections::{CollectionEntry, CollectionListPage},
        },
        handlers::shared::{extract_editor_locale, get_user_doc, has_read_access, render_page},
    },
    core::auth::{AuthUser, Claims},
};

/// GET /admin/collections — list all registered collections
pub async fn list_collections(
    State(state): State<AdminState>,
    headers: HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let user_doc = get_user_doc(&auth_user);

    let mut collections: Vec<CollectionEntry> = state
        .registry
        .collections
        .iter()
        .filter(|(_, def)| has_read_access(&state, def.access.read.as_deref(), user_doc))
        .map(|(slug, def)| CollectionEntry {
            slug: slug.to_string(),
            display_name: def.display_name().to_string(),
            field_count: def.fields.len(),
        })
        .collect();

    collections.sort_by(|a, b| a.slug.cmp(&b.slug));

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let claims_ref = claims.as_ref().map(|Extension(c)| c);

    let base = BasePageContext::for_handler(
        &state,
        claims_ref,
        &auth_user,
        PageMeta::new(PageType::CollectionList, "collections"),
    )
    .with_editor_locale(editor_locale.as_deref(), &state);

    let ctx = CollectionListPage { base, collections };

    render_page(&state, "collections/list", &ctx)
}
