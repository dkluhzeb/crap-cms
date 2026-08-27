//! Access-gated reads for display-label enrichment.
//!
//! The admin edit form must not surface the title, existence, or count of
//! relationship / join / upload targets the viewer cannot read. These label
//! lookups therefore apply the target collection's `published ∪ draft` view
//! filter (downgraded to the viewer's access) in SQL — the same scope a normal
//! read uses — instead of a raw, unscoped query.

use tracing::warn;

use crate::core::{CollectionDefinition, Document};
use crate::db::{Filter, FilterClause, FilterOp, FindQuery, query};
use crate::service::{ReadAccessCtx, RunnerReadHooks, requested_views, resolve_view_scope};

use super::EnrichCtx;

/// The view filters to AND into an enrichment read of `def` for the current
/// viewer — the published/draft union they may see (trashed rows are excluded by
/// the read itself). `None` when nothing is visible, so the caller surfaces
/// nothing rather than leaking labels.
fn view_filters(
    ctx: &EnrichCtx,
    slug: &str,
    def: &CollectionDefinition,
) -> Option<Vec<FilterClause>> {
    let hooks = RunnerReadHooks::new(&ctx.state.infra.hook_runner, ctx.conn, ctx.user, None);
    let read_ctx = ReadAccessCtx {
        def,
        slug,
        user: ctx.user,
        id: None,
        locale: None,
        operation: "find",
        ui_locale: None,
    };

    let scope = resolve_view_scope(&hooks, &read_ctx, requested_views(None, true))
        .inspect_err(|e| warn!("enrichment view-scope resolution for '{slug}' failed: {e}"))
        .ok()?;
    if !scope.is_anything_visible() {
        return None;
    }

    Some(scope.into_filters())
}

/// Gated single-target fetch by id for a display label: returns the document
/// only if the viewer may see it (readable, and not a draft they lack access to).
pub(in crate::admin::handlers::field_context) fn gated_find_by_id(
    ctx: &EnrichCtx,
    slug: &str,
    def: &CollectionDefinition,
    id: &str,
) -> Option<Document> {
    let mut filters = view_filters(ctx, slug, def)?;
    filters.push(FilterClause::Single(Filter {
        field: "id".to_string(),
        op: FilterOp::Equals(id.to_string()),
    }));

    let fq = FindQuery::builder().filters(filters).build();
    query::find(ctx.conn, slug, def, &fq, ctx.rel_locale_ctx)
        .inspect_err(|e| warn!("enrichment label read for '{slug}' failed: {e}"))
        .ok()?
        .into_iter()
        .next()
}

/// Gated reverse-lookup / list read for a display label set: `base_filters` AND
/// the viewer's view filters. Empty when nothing is visible — so a join field
/// never enumerates or counts rows of a collection the viewer cannot read.
pub(in crate::admin::handlers::field_context) fn gated_find(
    ctx: &EnrichCtx,
    slug: &str,
    def: &CollectionDefinition,
    base_filters: Vec<FilterClause>,
) -> Vec<Document> {
    let Some(mut filters) = view_filters(ctx, slug, def) else {
        return Vec::new();
    };
    filters.extend(base_filters);

    let fq = FindQuery::builder().filters(filters).build();
    query::find(ctx.conn, slug, def, &fq, ctx.rel_locale_ctx)
        .inspect_err(|e| warn!("enrichment label list read for '{slug}' failed: {e}"))
        .unwrap_or_default()
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::collections::HashMap;

    use rusqlite::Connection;

    use super::*;
    use crate::admin::handlers::field_context::enrich::test_helpers::make_test_state_with_deny;
    use crate::core::{CollectionDefinition, FieldDefinition, FieldType, Registry};

    /// Regression: enrichment label reads are access-gated. A target the viewer
    /// cannot read must NOT be surfaced as a label — closing the field-context
    /// enrichment leak. Driven by `default_deny` (no access rule + no viewer):
    /// deny → hidden, allow → surfaced.
    #[test]
    fn enrichment_label_read_is_access_gated() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY, title TEXT,
                _status TEXT DEFAULT 'published', created_at TEXT, updated_at TEXT
            );
            INSERT INTO posts (id, title) VALUES ('p1', 'Secret Title');",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];
        let reg = Registry::new();
        let errors = HashMap::new();

        // Read-denied viewer: the target's label must not surface.
        let deny = make_test_state_with_deny(true);
        let ctx_deny = EnrichCtx {
            state: &deny,
            non_default_locale: false,
            errors: &errors,
            conn: &conn,
            reg: &reg,
            rel_locale_ctx: None,
            user: None,
        };
        assert!(
            gated_find_by_id(&ctx_deny, "posts", &def, "p1").is_none(),
            "a target the viewer cannot read must not be labeled"
        );

        // Read-allowed viewer: the label surfaces as before.
        let allow = make_test_state_with_deny(false);
        let ctx_allow = EnrichCtx {
            state: &allow,
            non_default_locale: false,
            errors: &errors,
            conn: &conn,
            reg: &reg,
            rel_locale_ctx: None,
            user: None,
        };
        assert!(
            gated_find_by_id(&ctx_allow, "posts", &def, "p1").is_some(),
            "a readable target is still labeled"
        );
    }
}
