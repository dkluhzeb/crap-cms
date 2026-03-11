use axum::{Extension, extract::State, response::IntoResponse};

use crate::admin::AdminState;
use crate::admin::context::{ContextBuilder, PageType};
use crate::admin::handlers::shared::{extract_editor_locale, render_or_error};
use crate::core::auth::Claims;

/// GET /admin/collections — list all registered collections
pub async fn list_collections(
    State(state): State<AdminState>,
    headers: axum::http::HeaderMap,
    claims: Option<Extension<Claims>>,
) -> impl IntoResponse {
    let mut collections = Vec::new();
    for (slug, def) in &state.registry.collections {
        collections.push(serde_json::json!({
            "slug": slug,
            "display_name": def.display_name(),
            "field_count": def.fields.len(),
        }));
    }
    collections.sort_by(|a, b| a["slug"].as_str().cmp(&b["slug"].as_str()));

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let claims_ref = claims.as_ref().map(|Extension(c)| c);
    let data = ContextBuilder::new(&state, claims_ref)
        .editor_locale(editor_locale.as_deref(), &state.config.locale)
        .page(PageType::CollectionList, "Collections")
        .set("collections", serde_json::json!(collections))
        .build();

    let data = state.hook_runner.run_before_render(data);

    render_or_error(&state, "collections/list", &data).into_response()
}
