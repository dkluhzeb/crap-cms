use axum::{Extension, extract::State, http::HeaderMap, response::Response};

use crate::admin::handlers::shared::HxNav;
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
    hx: HxNav,
    headers: HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let user_doc = get_user_doc(auth_user.as_ref());

    let mut collections: Vec<CollectionEntry> = state
        .infra
        .registry
        .collections
        .iter()
        .filter(|(_, def)| has_read_access(&state, def.access.read.as_ref(), user_doc, &def.slug))
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
        auth_user.as_ref(),
        PageMeta::new(PageType::CollectionList, "collections"),
    )
    .with_editor_locale(editor_locale.as_deref(), &state);

    let ctx = CollectionListPage { base, collections };

    render_page(&state, hx, "collections/list", &ctx)
}
