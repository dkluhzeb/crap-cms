//! Access-gated reads for display-label enrichment.
//!
//! The admin edit form must not surface the title, existence, or count of
//! relationship / join / upload targets the viewer cannot read. These label
//! lookups therefore apply the target collection's `published ∪ draft` view
//! filter (downgraded to the viewer's access) in SQL — the same scope a normal
//! read uses — instead of a raw, unscoped query.
//!
//! A forward reference whose target is not visible keeps its stored id — part
//! of the document being edited, not of the target — as an unlabelled
//! "unavailable" item, so saving the form never drops the reference.

use std::slice;

use tracing::warn;

use crate::{
    core::{CollectionDefinition, Document, JoinConfig},
    db::{
        Filter, FilterClause, FilterOp, FindQuery, LocaleContext,
        query::{self, PopulateOpts, join_children},
    },
    service::{
        ReadAccessCtx, ReadStripArgs, RunnerReadHooks, helpers::strip_unreadable_docs,
        hooks::ReadHooksJoinGuard, join_child_readable, requested_views, resolve_view_scope,
    },
};

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
    let mut doc = query::find(ctx.conn, slug, def, &fq, ctx.rel_locale_ctx)
        .inspect_err(|e| warn!("enrichment label read for '{slug}' failed: {e}"))
        .ok()?
        .into_iter()
        .next()?;

    strip_label_docs(ctx, slug, def, slice::from_mut(&mut doc));

    Some(doc)
}

/// Strip what the viewer may not read from label documents, as every read
/// does: read-denied fields — each document its own `ctx.document` — and hidden
/// fields. The read-access strip runs as one batch, so the Lua VM is taken once
/// for the whole list. A label whose title field is stripped falls back to the id.
fn strip_label_docs(
    ctx: &EnrichCtx,
    slug: &str,
    def: &CollectionDefinition,
    docs: &mut [Document],
) {
    let hooks = RunnerReadHooks::new(&ctx.state.infra.hook_runner, ctx.conn, ctx.user, None);
    let locale = ctx.rel_locale_ctx.map(LocaleContext::access_locale);

    strip_unreadable_docs(
        &hooks,
        &ReadStripArgs::builder(&def.fields, slug)
            .user(ctx.user)
            .locale(locale)
            .build(),
        docs,
    );
}

/// The children a join lists for the edited document `parent_id`, labelled
/// for the viewer: the very lookup a read populates the join with — the
/// target's views (drafts shown through the draft view), children whose `on`
/// value the viewer may not read left out before the join's `limit` cuts the
/// list — then stripped of what the viewer may not read. Empty when nothing
/// is visible or the lookup fails.
pub(in crate::admin::handlers::field_context) fn gated_join_children(
    ctx: &EnrichCtx,
    (jc, target_def): (&JoinConfig, &CollectionDefinition),
    parent_id: &str,
) -> Vec<Document> {
    let hooks = RunnerReadHooks::new(&ctx.state.infra.hook_runner, ctx.conn, ctx.user, None);
    let guard = ReadHooksJoinGuard::new(&hooks);

    let mut opts = PopulateOpts::new(0).join_access(&guard, ctx.user);
    if let Some(locale_ctx) = ctx.rel_locale_ctx {
        opts = opts.locale_ctx(locale_ctx);
    }

    let mut docs = join_children(ctx.conn, ctx.reg, (jc, parent_id), &opts)
        .inspect_err(|e| warn!("enrichment join read for '{}' failed: {e}", jc.collection))
        .unwrap_or_default();

    strip_label_docs(ctx, &jc.collection, target_def, &mut docs);

    // The lookup already left these out; kept as a backstop so a label can
    // never name a child whose `on` value the viewer may not read.
    docs.retain(|doc| join_child_readable(jc, &doc.fields));

    docs
}

/// How many visible rows match `base_filters`: they AND the viewer's view
/// filters (the published/draft union), counted in SQL. `None` when nothing is visible
/// or the count fails — the caller then shows no total rather than a wrong one.
pub(in crate::admin::handlers::field_context) fn gated_count(
    ctx: &EnrichCtx,
    (slug, def): (&str, &CollectionDefinition),
    base_filters: Vec<FilterClause>,
) -> Option<usize> {
    let mut filters = view_filters(ctx, slug, def)?;
    filters.extend(base_filters);

    let total = query::count(ctx.conn, slug, def, &filters, ctx.rel_locale_ctx)
        .inspect_err(|e| warn!("enrichment count for '{slug}' failed: {e}"))
        .ok()?;

    usize::try_from(total).ok()
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::collections::HashMap;

    use rusqlite::Connection;

    use super::*;
    use crate::{
        admin::handlers::field_context::enrich::test_helpers::make_test_state_with_deny,
        core::{FieldDefinition, FieldType, Registry, RelationshipConfig},
    };

    /// Regression: a label read skipped the read strips, so a hidden (or
    /// read-denied) title field still named the selected item.
    #[test]
    fn a_hidden_title_is_not_read_for_a_label() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY, _revision INTEGER NOT NULL DEFAULT 0, title TEXT,
                _status TEXT DEFAULT 'published', created_at TEXT, updated_at TEXT
            );
            INSERT INTO posts (id, title) VALUES ('p1', 'Secret Title');",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .hidden(true)
                .build(),
        ];
        let reg = Registry::new();
        let errors = HashMap::new();
        let state = make_test_state_with_deny(false);
        let ctx = EnrichCtx {
            state: &state,
            non_default_locale: false,
            errors: &errors,
            conn: &conn,
            reg: &reg,
            rel_locale_ctx: None,
            user: None,
            doc_id: None,
            ancestor_readonly: false,
        };

        let doc = gated_find_by_id(&ctx, "posts", &def, "p1").expect("a readable target");

        assert_eq!(doc.get_str("title"), None);
    }

    /// A join's label list strips every document it returns, not only the
    /// first: the hidden title field names none of the listed items.
    #[test]
    fn a_hidden_title_is_not_read_for_any_join_label() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY, _revision INTEGER NOT NULL DEFAULT 0, title TEXT, author TEXT,
                _status TEXT DEFAULT 'published', created_at TEXT, updated_at TEXT
            );
            INSERT INTO posts (id, title, author) VALUES ('p1', 'Secret One', 'au1');
            INSERT INTO posts (id, title, author) VALUES ('p2', 'Secret Two', 'au1');",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .hidden(true)
                .build(),
            FieldDefinition::builder("author", FieldType::Relationship)
                .relationship(RelationshipConfig::new("authors", false))
                .build(),
        ];
        let mut reg = Registry::new();
        reg.register_collection(def.clone());
        let errors = HashMap::new();
        let state = make_test_state_with_deny(false);
        let ctx = EnrichCtx {
            state: &state,
            non_default_locale: false,
            errors: &errors,
            conn: &conn,
            reg: &reg,
            rel_locale_ctx: None,
            user: None,
            doc_id: None,
            ancestor_readonly: false,
        };

        let join = JoinConfig::new("posts", "author");
        let docs = gated_join_children(&ctx, (&join, &def), "au1");

        assert_eq!(docs.len(), 2, "both readable targets are listed");
        for doc in &docs {
            assert_eq!(doc.get_str("title"), None, "title leaked for {}", doc.id);
        }
    }

    /// Regression: enrichment label reads are access-gated. A target the viewer
    /// cannot read must NOT be surfaced as a label — closing the field-context
    /// enrichment leak. Driven by `default_deny` (no access rule + no viewer):
    /// deny → hidden, allow → surfaced.
    #[test]
    fn enrichment_label_read_is_access_gated() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY, _revision INTEGER NOT NULL DEFAULT 0, title TEXT,
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
            doc_id: None,
            ancestor_readonly: false,
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
            doc_id: None,
            ancestor_readonly: false,
        };
        assert!(
            gated_find_by_id(&ctx_allow, "posts", &def, "p1").is_some(),
            "a readable target is still labeled"
        );
    }
}
