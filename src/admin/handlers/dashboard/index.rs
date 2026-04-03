//! Dashboard handler showing collection/global cards with document counts.

use axum::{Extension, extract::State, http::HeaderMap, response::Html};
use serde_json::{Value, json};
use tracing::error;

use crate::{
    admin::{
        AdminState,
        context::{ContextBuilder, PageType},
        handlers::shared::{extract_editor_locale, get_user_doc, has_read_access},
    },
    core::{
        Document,
        auth::{AuthUser, Claims},
    },
    db::{BoxedConnection, DbConnection, ops::count_documents},
};

/// Fetch the most recent `updated_at` value from a table.
fn last_updated(conn: &BoxedConnection, table: &str, where_clause: &str) -> Option<String> {
    let sql = format!(
        "SELECT MAX(updated_at) AS last_updated FROM \"{}\"{}",
        table, where_clause
    );

    conn.query_one(&sql, &[])
        .ok()
        .flatten()
        .and_then(|r| r.get_opt_string("last_updated").ok().flatten())
}

/// Build dashboard cards for all readable collections.
fn build_collection_cards(
    state: &AdminState,
    conn: &BoxedConnection,
    user_doc: Option<&Document>,
) -> Vec<Value> {
    let mut cards: Vec<Value> = state
        .registry
        .collections
        .iter()
        .filter(|(_, def)| has_read_access(state, def.access.read.as_deref(), user_doc))
        .map(|(slug, def)| {
            let count = count_documents(&state.pool, slug, def, &[], None).unwrap_or(0);

            json!({
                "slug": slug,
                "display_name": def.display_name(),
                "singular_name": def.singular_name(),
                "count": count,
                "last_updated": last_updated(conn, slug, ""),
                "is_auth": def.is_auth_collection(),
                "is_upload": def.upload.is_some(),
                "has_versions": def.has_versions(),
            })
        })
        .collect();

    cards.sort_by(|a, b| a["slug"].as_str().cmp(&b["slug"].as_str()));

    cards
}

/// Build dashboard cards for all readable globals.
fn build_global_cards(
    state: &AdminState,
    conn: &BoxedConnection,
    user_doc: Option<&Document>,
) -> Vec<Value> {
    let mut cards: Vec<Value> = state
        .registry
        .globals
        .iter()
        .filter(|(_, def)| has_read_access(state, def.access.read.as_deref(), user_doc))
        .map(|(slug, def)| {
            let table = format!("_global_{}", slug);

            json!({
                "slug": slug,
                "display_name": def.display_name(),
                "last_updated": last_updated(conn, &table, " WHERE id = 'default'"),
                "has_versions": def.has_versions(),
            })
        })
        .collect();

    cards.sort_by(|a, b| a["slug"].as_str().cmp(&b["slug"].as_str()));

    cards
}

/// Render the admin dashboard with collection and global summary cards.
pub async fn index(
    State(state): State<AdminState>,
    headers: HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> Html<String> {
    let conn = match state.pool.get() {
        Ok(c) => c,
        Err(_) => return Html("<h1>Database error</h1>".to_string()),
    };

    let user_doc = get_user_doc(&auth_user);
    let collection_cards = build_collection_cards(&state, &conn, user_doc);
    let global_cards = build_global_cards(&state, &conn, user_doc);

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
            error!("Template render error: {}", e);

            Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
        }
    }
}
