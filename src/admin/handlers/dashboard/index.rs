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
    core::{AuthUser, Claims, Document},
    db::{BoxedConnection, DbConnection, query::helpers::global_table},
    hooks::HookRunner,
    service::{CollectionStats, RunnerReadHooks, ServiceContext, collection_stats},
};

/// Fetch the most recent `updated_at` value from a table.
fn last_updated(conn: &BoxedConnection, table: &str, where_clause: &str) -> Option<String> {
    let sql = format!("SELECT MAX(updated_at) AS last_updated FROM \"{table}\"{where_clause}");

    conn.query_one(&sql, &[])
        .ok()
        .flatten()
        .and_then(|r| r.get_opt_string("last_updated").ok().flatten())
}

/// Build dashboard cards for all readable collections.
///
/// The count and "last updated" come from [`collection_stats`], which scopes
/// them to the viewer's live view (published ∪ draft, downgraded to what they
/// may see, trashed rows excluded) — so neither figure reveals drafts or
/// other-owner rows the viewer cannot access.
fn build_collection_cards(
    state: &AdminState,
    conn: &BoxedConnection,
    runner: &HookRunner,
    user_doc: Option<&Document>,
) -> Vec<CollectionCard> {
    let hooks = RunnerReadHooks::new(runner, conn, user_doc, None);

    let mut cards: Vec<CollectionCard> = state
        .registry
        .collections
        .iter()
        .filter(|(_, def)| has_read_access(state, def.access.read.as_ref(), user_doc, &def.slug))
        .map(|(slug, def)| {
            let ctx = ServiceContext::collection(slug, def)
                .conn(conn)
                .read_hooks(&hooks)
                .user(user_doc)
                .build();
            let card_stats = collection_stats(&ctx, true).unwrap_or(CollectionStats {
                count: 0,
                last_updated: None,
            });

            CollectionCard {
                slug: slug.to_string(),
                display_name: def.display_name().to_string(),
                singular_name: def.singular_name().to_string(),
                count: card_stats.count,
                last_updated: card_stats.last_updated,
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
        .filter(|(_, def)| has_read_access(state, def.access.read.as_ref(), user_doc, &def.slug))
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
    let Ok(conn) = state.pool.get() else {
        return crate::admin::handlers::shared::render_or_error(
            &state,
            "errors/500",
            &serde_json::json!({"message": "Database error"}),
        );
    };

    let user_doc = get_user_doc(auth_user.as_ref());
    let collection_cards = build_collection_cards(&state, &conn, &state.hook_runner, user_doc);
    let global_cards = build_global_cards(&state, &conn, user_doc);

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let claims_ref = claims.as_ref().map(|Extension(c)| c);

    let base = BasePageContext::for_handler(
        &state,
        claims_ref,
        auth_user.as_ref(),
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
