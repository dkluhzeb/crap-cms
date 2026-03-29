//! Dashboard handler showing collection/global cards with document counts.

use axum::{Extension, extract::State, http::HeaderMap, response::Html};
use serde_json::{Value, json};

use crate::{
    admin::{
        AdminState,
        context::{ContextBuilder, PageType},
        handlers::shared::{extract_editor_locale, get_user_doc},
    },
    core::auth::{AuthUser, Claims},
    db::{DbConnection, ops::count_documents, query::AccessResult},
};

/// Render the admin dashboard with collection and global summary cards.
pub async fn index(
    State(state): State<AdminState>,
    headers: HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> Html<String> {
    let mut collection_cards = Vec::new();
    let mut global_cards = Vec::new();
    {
        let conn = state.pool.get().ok();
        let user_doc = get_user_doc(&auth_user);

        for (slug, def) in &state.registry.collections {
            // Skip collections the user cannot read
            if !has_read_access(&state, def.access.read.as_deref(), user_doc) {
                continue;
            }

            let count = count_documents(&state.pool, slug, def, &[], None).unwrap_or(0);
            let last_updated = conn.as_ref().and_then(|c| {
                c.query_one(
                    &format!("SELECT MAX(updated_at) AS last_updated FROM \"{}\"", slug),
                    &[],
                )
                .ok()
                .flatten()
                .and_then(|r| r.get_opt_string("last_updated").ok().flatten())
            });

            collection_cards.push(json!({
                "slug": slug,
                "display_name": def.display_name(),
                "singular_name": def.singular_name(),
                "count": count,
                "last_updated": last_updated,
                "is_auth": def.is_auth_collection(),
                "is_upload": def.upload.is_some(),
                "has_versions": def.has_versions(),
            }));
        }

        for (slug, def) in &state.registry.globals {
            // Skip globals the user cannot read
            if !has_read_access(&state, def.access.read.as_deref(), user_doc) {
                continue;
            }

            let table_name = format!("_global_{}", slug);
            let last_updated = conn.as_ref().and_then(|c| {
                c.query_one(
                    &format!(
                        "SELECT updated_at AS last_updated FROM \"{}\" WHERE id = 'default'",
                        table_name
                    ),
                    &[],
                )
                .ok()
                .flatten()
                .and_then(|r| r.get_opt_string("last_updated").ok().flatten())
            });

            global_cards.push(json!({
                "slug": slug,
                "display_name": def.display_name(),
                "last_updated": last_updated,
                "has_versions": def.has_versions(),
            }));
        }
    }

    collection_cards.sort_by(|a, b| a["slug"].as_str().cmp(&b["slug"].as_str()));
    global_cards.sort_by(|a, b| a["slug"].as_str().cmp(&b["slug"].as_str()));

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let claims_ref = claims.as_ref().map(|Extension(c)| c);

    let data = ContextBuilder::new(&state, claims_ref)
        .locale_from_auth(&auth_user)
        .filter_nav_by_access(&state, &auth_user)
        .editor_locale(editor_locale.as_deref(), &state.config.locale)
        .page(PageType::Dashboard, "dashboard")
        .set("collection_cards", Value::Array(collection_cards))
        .set("global_cards", Value::Array(global_cards))
        .build();

    let data = state.hook_runner.run_before_render(data);

    match state.render("dashboard/index", &data) {
        Ok(html) => Html(html),
        Err(e) => {
            tracing::error!("Template render error: {}", e);
            Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
        }
    }
}

/// Quick read-access check for dashboard card visibility.
/// Returns true if the user is allowed to see this collection/global.
fn has_read_access(
    state: &AdminState,
    access_ref: Option<&str>,
    user_doc: Option<&crate::core::Document>,
) -> bool {
    if access_ref.is_none() {
        return !state.config.access.default_deny;
    }

    let mut conn = match state.pool.get() {
        Ok(c) => c,
        Err(_) => return false,
    };

    let tx = match conn.transaction() {
        Ok(t) => t,
        Err(_) => return false,
    };

    let result = state
        .hook_runner
        .check_access(access_ref, user_doc, None, None, &tx);

    if let Err(e) = tx.commit() {
        tracing::warn!("tx commit failed: {e}");
    }

    matches!(
        result,
        Ok(AccessResult::Allowed | AccessResult::Constrained(_))
    )
}
