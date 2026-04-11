use axum::{Extension, extract::State, http::HeaderMap, response::Response};
use serde_json::json;

use crate::{
    admin::{
        AdminState,
        context::{ContextBuilder, PageType},
        handlers::shared::{extract_editor_locale, get_user_doc, has_read_access, render_or_error},
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
    let mut collections = Vec::new();

    for (slug, def) in &state.registry.collections {
        // Skip collections the user cannot read
        if !has_read_access(&state, def.access.read.as_deref(), user_doc) {
            continue;
        }

        collections.push(json!({
            "slug": slug,
            "display_name": def.display_name(),
            "field_count": def.fields.len(),
        }));
    }

    collections.sort_by(|a, b| a["slug"].as_str().cmp(&b["slug"].as_str()));

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let claims_ref = claims.as_ref().map(|Extension(c)| c);

    let data = ContextBuilder::new(&state, claims_ref)
        .filter_nav_by_access(&state, &auth_user)
        .editor_locale(editor_locale.as_deref(), &state.config.locale)
        .page(PageType::CollectionList, "collections")
        .set("collections", json!(collections))
        .build();

    let data = state.hook_runner.run_before_render(data);

    render_or_error(&state, "collections/list", &data)
}
