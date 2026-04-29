//! Dashboard handler showing collection/global cards with document counts.

use axum::{Extension, extract::State, http::HeaderMap, response::Response};

use crate::{
    admin::{
        AdminState,
        context::{
            BasePageContext, PageMeta, PageType,
            page::dashboard::{CollectionCard, DashboardPage, GlobalCard},
        },
        handlers::shared::{extract_editor_locale, get_user_doc, has_read_access, render_page},
    },
    core::{
        Document,
        auth::{AuthUser, Claims},
    },
    db::{BoxedConnection, DbConnection, ops::count_documents, query::helpers::global_table},
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
) -> Vec<CollectionCard> {
    let mut cards: Vec<CollectionCard> = state
        .registry
        .collections
        .iter()
        .filter(|(_, def)| has_read_access(state, def.access.read.as_deref(), user_doc))
        .map(|(slug, def)| {
            let count = count_documents(&state.pool, slug, def, &[], None).unwrap_or(0);

            CollectionCard {
                slug: slug.to_string(),
                display_name: def.display_name().to_string(),
                singular_name: def.singular_name().to_string(),
                count,
                last_updated: last_updated(conn, slug, ""),
                is_auth: def.is_auth_collection(),
                is_upload: def.upload.is_some(),
                has_versions: def.has_versions(),
            }
        })
        .collect();

    cards.sort_by(|a, b| a.slug.cmp(&b.slug));

    cards
}

/// Build dashboard cards for all readable globals.
fn build_global_cards(
    state: &AdminState,
    conn: &BoxedConnection,
    user_doc: Option<&Document>,
) -> Vec<GlobalCard> {
    let mut cards: Vec<GlobalCard> = state
        .registry
        .globals
        .iter()
        .filter(|(_, def)| has_read_access(state, def.access.read.as_deref(), user_doc))
        .map(|(slug, def)| {
            let table = global_table(slug);

            GlobalCard {
                slug: slug.to_string(),
                display_name: def.display_name().to_string(),
                last_updated: last_updated(conn, &table, " WHERE id = 'default'"),
                has_versions: def.has_versions(),
            }
        })
        .collect();

    cards.sort_by(|a, b| a.slug.cmp(&b.slug));

    cards
}

/// Render the admin dashboard with collection and global summary cards.
pub async fn index(
    State(state): State<AdminState>,
    headers: HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let conn = match state.pool.get() {
        Ok(c) => c,
        Err(_) => {
            return crate::admin::handlers::shared::render_or_error(
                &state,
                "errors/500",
                &serde_json::json!({"message": "Database error"}),
            );
        }
    };

    let user_doc = get_user_doc(&auth_user);
    let collection_cards = build_collection_cards(&state, &conn, user_doc);
    let global_cards = build_global_cards(&state, &conn, user_doc);

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let claims_ref = claims.as_ref().map(|Extension(c)| c);

    let base = BasePageContext::for_handler(
        &state,
        claims_ref,
        &auth_user,
        PageMeta::new(PageType::Dashboard, "dashboard"),
    )
    .with_editor_locale(editor_locale.as_deref(), &state);

    let ctx = DashboardPage {
        base,
        collection_cards,
        global_cards,
    };

    render_page(&state, "dashboard/index", &ctx)
}
