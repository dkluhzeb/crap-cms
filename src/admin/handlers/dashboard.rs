//! Dashboard handler showing collection/global cards with document counts.

use axum::{
    extract::State,
    response::Html,
    Extension,
};

use crate::admin::AdminState;
use crate::admin::context::{ContextBuilder, PageType};
use crate::core::auth::Claims;

/// Render the admin dashboard with collection and global summary cards.
pub async fn index(
    State(state): State<AdminState>,
    claims: Option<Extension<Claims>>,
) -> Html<String> {
    let mut collection_cards = Vec::new();
    let mut global_cards = Vec::new();
    {
        let reg = match state.registry.read() {
            Ok(r) => r,
            Err(e) => return Html(format!("<h1>Error</h1><pre>Registry lock poisoned: {}</pre>", e)),
        };
        for (slug, def) in &reg.collections {
            let count = crate::db::ops::count_documents(&state.pool, slug, def, &[], None)
                .unwrap_or(0);
            collection_cards.push(serde_json::json!({
                "slug": slug,
                "display_name": def.display_name(),
                "singular_name": def.singular_name(),
                "count": count,
            }));
        }
        for (slug, def) in &reg.globals {
            global_cards.push(serde_json::json!({
                "slug": slug,
                "display_name": def.display_name(),
            }));
        }
    }
    collection_cards.sort_by(|a, b| a["slug"].as_str().cmp(&b["slug"].as_str()));
    global_cards.sort_by(|a, b| a["slug"].as_str().cmp(&b["slug"].as_str()));

    let claims_ref = claims.as_ref().map(|Extension(c)| c);
    let data = ContextBuilder::new(&state, claims_ref)
        .page(PageType::Dashboard, "Dashboard")
        .set("collection_cards", serde_json::Value::Array(collection_cards))
        .set("global_cards", serde_json::Value::Array(global_cards))
        // Backward compat: dashboard template uses {{#each collections}} and {{#each globals}}
        // These are now the card data (with counts), distinct from nav.collections
        .build();

    let data = state.hook_runner.run_before_render(data);

    match state.render("dashboard/index", &data) {
        Ok(html) => Html(html),
        Err(e) => Html(format!("<h1>Error</h1><pre>{}</pre>", e)),
    }
}
