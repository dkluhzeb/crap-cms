//! Join field enrichment: the reverse-lookup items a join lists.

use std::slice;

use crate::{
    admin::{
        context::field::{JoinField, JoinItem},
        handlers::field_context::enrich::{EnrichCtx, gated_count, gated_join_children},
    },
    core::{CollectionDefinition, Document, FieldDefinition, JoinConfig},
    db::{Filter, FilterClause, FilterOp, LocaleContext},
    service::{RunnerReadHooks, ServiceContext, unreadable_query_paths},
};

/// The item a join lists for `doc`: labelled by the target's title field, or
/// by its id when there is none (or the viewer may not read it).
fn join_item(doc: &Document, title_field: Option<&str>) -> JoinItem {
    let label = title_field
        .and_then(|f| doc.get_str(f))
        .unwrap_or(&doc.id)
        .to_string();

    JoinItem {
        id: doc.id.to_string(),
        label,
    }
}

/// Whether the viewer may read the `on` value of every child — asked of the
/// one query-path oracle a filter on `on` would face, which probes through the
/// real read strip: `hidden` and `access.read` on the field or on anything
/// holding it, and a rule that needs the row to decide, all answer no. Only
/// then does a SQL count of the referencing rows equal what the join would
/// list without its limit; otherwise a count would reveal children the list
/// leaves out.
fn on_value_always_readable(
    ctx: &EnrichCtx,
    jc: &JoinConfig,
    target_def: &CollectionDefinition,
) -> bool {
    let hooks = RunnerReadHooks::new(&ctx.state.infra.hook_runner, ctx.conn, ctx.user, None);
    let service = ServiceContext::collection(&jc.collection, target_def)
        .conn(ctx.conn)
        .read_hooks(&hooks)
        .user(ctx.user)
        .build();
    let locale = ctx.rel_locale_ctx.map(LocaleContext::access_locale);

    unreadable_query_paths(&service, locale, slice::from_ref(&jc.on))
        .is_ok_and(|unreadable| unreadable.is_empty())
}

/// The number of visible documents referencing the edited one, when it
/// exceeds the `listed` the join shows — the list stops at the join's `limit`.
/// `None` when every one is listed, or when counting could reveal a child the
/// list leaves out (see [`on_value_always_readable`]).
fn hidden_total(
    ctx: &EnrichCtx,
    (jc, target_def): (&JoinConfig, &CollectionDefinition),
    doc_id: &str,
    listed: usize,
) -> Option<usize> {
    let limit = usize::try_from(jc.effective_limit()).unwrap_or(usize::MAX);

    if listed < limit || !on_value_always_readable(ctx, jc, target_def) {
        return None;
    }

    let base = vec![FilterClause::Single(Filter {
        field: jc.on.clone(),
        op: FilterOp::Equals(doc_id.to_string()),
    })];

    gated_count(ctx, (&jc.collection, target_def), base).filter(|&total| total > listed)
}

/// Enrich a Join field context — at the top level, in a layout wrapper or in a
/// group — with the reverse-lookup items of the edited document
/// (`ctx.doc_id`): exactly the children a read populates the join with — at
/// most its `limit`, in the target's default order — and, when more exist than
/// it lists, how many there are.
///
/// Access-gated so the join never enumerates, labels, or counts rows the
/// viewer cannot read — nor rows whose `on` value the viewer cannot read,
/// since a row's presence in the join is that value.
pub(super) fn enrich_join(jf: &mut JoinField, field_def: &FieldDefinition, ctx: &EnrichCtx) {
    let Some(jc) = &field_def.join else {
        return;
    };
    let Some(doc_id) = ctx.doc_id else {
        return;
    };
    let Some(target_def) = ctx.reg.get_collection(&jc.collection) else {
        return;
    };

    let docs = gated_join_children(ctx, (jc, target_def), doc_id);

    let items: Vec<JoinItem> = docs
        .iter()
        .map(|doc| join_item(doc, target_def.title_field()))
        .collect();

    jf.join_count = Some(items.len());
    jf.join_total = hidden_total(ctx, (jc, target_def), doc_id, items.len());
    jf.join_items = Some(items);
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::{collections::HashMap, path::PathBuf};

    use rusqlite::Connection;

    use super::*;
    use crate::{
        admin::{
            context::field::FieldContext,
            handlers::field_context::enrich::{
                enrich_nested_fields,
                test_helpers::{make_test_state_with_deny, make_test_state_with_hooks},
            },
        },
        core::{FieldType, HookRef, Registry, RelationshipConfig},
    };

    /// `posts` (`title`, `author` → `authors`) holding `p1` and `p2` by author
    /// `au1`; `author` is `hidden` when `hide_author` is set.
    fn registry(hide_author: bool) -> Registry {
        let mut author = FieldDefinition::builder("author", FieldType::Relationship)
            .relationship(RelationshipConfig::new("authors", false))
            .build();
        author.hidden = hide_author;

        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            author,
        ];

        let mut registry = Registry::new();
        registry.register_collection(posts);
        registry
    }

    fn posts_table() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY, _revision INTEGER NOT NULL DEFAULT 0, title TEXT, author TEXT,
                _status TEXT DEFAULT 'published', created_at TEXT, updated_at TEXT
            );
            INSERT INTO posts (id, title, author) VALUES ('p1', 'One', 'au1');
            INSERT INTO posts (id, title, author) VALUES ('p2', 'Two', 'au1');",
        )
        .unwrap();

        conn
    }

    /// Enrich `authors.recent_posts` (joining `posts` on `author`, listing at
    /// most `limit`) for `au1`.
    fn enriched(reg: &Registry, limit: Option<u32>) -> JoinField {
        let conn = posts_table();
        let errors = HashMap::new();
        let state = make_test_state_with_deny(false);
        let ctx = EnrichCtx {
            state: &state,
            non_default_locale: false,
            errors: &errors,
            conn: &conn,
            reg,
            rel_locale_ctx: None,
            user: None,
            doc_id: Some("au1"),
            ancestor_readonly: false,
        };

        let mut join = JoinConfig::new("posts", "author");
        join.limit = limit;
        let field = FieldDefinition::builder("recent_posts", FieldType::Join)
            .join(join)
            .build();

        let mut jf = JoinField::default();
        enrich_join(&mut jf, &field, &ctx);

        jf
    }

    #[test]
    fn a_join_lists_and_counts_the_documents_that_reference_this_one() {
        let jf = enriched(&registry(false), None);

        let ids: Vec<&str> = jf
            .join_items
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|item| item.id.as_str())
            .collect();
        assert_eq!(ids.len(), 2, "{ids:?}");
        assert!(ids.contains(&"p1") && ids.contains(&"p2"), "{ids:?}");
        assert_eq!(jf.join_count, Some(2));
    }

    /// Regression: the join listed (and counted) every document referencing
    /// this one even when the viewer could not read the referencing field, so
    /// the list revealed that field's value.
    #[test]
    fn a_join_on_an_unreadable_field_lists_and_counts_nothing() {
        let jf = enriched(&registry(true), None);

        assert_eq!(jf.join_items.as_deref().map(<[JoinItem]>::len), Some(0));
        assert_eq!(jf.join_count, Some(0));
    }

    /// Regression: the admin join listed every referencing document,
    /// unbounded. It lists at most the join's `limit`, like a read.
    #[test]
    fn a_join_lists_at_most_its_limit() {
        let jf = enriched(&registry(false), Some(1));

        assert_eq!(jf.join_items.as_deref().map(<[JoinItem]>::len), Some(1));
        assert_eq!(jf.join_count, Some(1));
    }

    /// Regression: a join cut off at its `limit` showed the number it listed
    /// as if it were all of them. The total of visible referencing documents
    /// rides along whenever more exist than are listed.
    #[test]
    fn a_join_cut_off_at_its_limit_reports_the_total() {
        let jf = enriched(&registry(false), Some(1));
        assert_eq!(jf.join_total, Some(2));

        let all_listed = enriched(&registry(false), None);
        assert_eq!(all_listed.join_total, None, "nothing left out");
    }

    /// The example config, whose `access.*` modules the rules below name.
    fn example_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("example")
    }

    /// `posts` holding `author` (→ `authors`) readable under `rule`, when set.
    fn posts_with_author_rule(rule: Option<&str>) -> CollectionDefinition {
        let mut author = FieldDefinition::builder("author", FieldType::Relationship)
            .relationship(RelationshipConfig::new("authors", false))
            .build();
        author.access.read = rule.map(HookRef::new);

        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("public", FieldType::Checkbox).build(),
            author,
        ];
        posts
    }

    /// Regression: the "N of M" count judged the `on` field's readability with
    /// its own walk over the definition, a second copy of the query-path
    /// oracle's rule. It now asks the oracle, so a rule that needs the row to
    /// decide — or denies this viewer — reports no count, and only an ungated
    /// `on` does.
    #[test]
    fn the_on_value_is_always_readable_only_when_the_query_oracle_says_so() {
        let state = make_test_state_with_hooks(&example_dir());
        let conn = posts_table();
        let errors = HashMap::new();
        let reg = Registry::new();
        let ctx = EnrichCtx {
            state: &state,
            non_default_locale: false,
            errors: &errors,
            conn: &conn,
            reg: &reg,
            rel_locale_ctx: None,
            user: None,
            doc_id: Some("au1"),
            ancestor_readonly: false,
        };
        let on = JoinConfig::new("posts", "author");

        let open = posts_with_author_rule(None);
        assert!(on_value_always_readable(&ctx, &on, &open));

        let denied = posts_with_author_rule(Some("access.admin_only"));
        assert!(!on_value_always_readable(&ctx, &on, &denied));

        let per_row = posts_with_author_rule(Some("access.field_read_if_public"));
        assert!(!on_value_always_readable(&ctx, &on, &per_row));

        let mut hidden = posts_with_author_rule(None);
        hidden.fields[2].hidden = true;
        assert!(!on_value_always_readable(&ctx, &on, &hidden));
    }

    /// Regression: the join applied its `limit` in SQL before dropping the
    /// children whose `on` value the viewer may not read, so with the five
    /// newest posts private it listed nothing although three public posts
    /// reference the author. The limit now counts listed children — the two
    /// newest public posts — exactly as a read populates the join; no total
    /// is reported, since counting would include the private ones.
    #[test]
    fn a_join_limit_counts_only_children_whose_on_value_is_readable() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY, _revision INTEGER NOT NULL DEFAULT 0, title TEXT, public INTEGER, author TEXT,
                _status TEXT DEFAULT 'published', created_at TEXT, updated_at TEXT
            );",
        )
        .unwrap();
        for (id, public, day) in [
            ("p1", 0, 10),
            ("p2", 0, 9),
            ("p3", 0, 8),
            ("p4", 0, 7),
            ("p5", 0, 6),
            ("p6", 1, 3),
            ("p7", 1, 2),
            ("p8", 1, 1),
        ] {
            conn.execute_batch(&format!(
                "INSERT INTO posts (id, title, public, author, created_at, updated_at) \
                 VALUES ('{id}', '{id}', {public}, 'au1', '2024-01-{day:02}', '2024-01-{day:02}');"
            ))
            .unwrap();
        }

        let mut reg = Registry::new();
        reg.register_collection(posts_with_author_rule(Some("access.field_read_if_public")));
        let state = make_test_state_with_hooks(&example_dir());
        let errors = HashMap::new();
        let ctx = EnrichCtx {
            state: &state,
            non_default_locale: false,
            errors: &errors,
            conn: &conn,
            reg: &reg,
            rel_locale_ctx: None,
            user: None,
            doc_id: Some("au1"),
            ancestor_readonly: false,
        };

        let mut join = JoinConfig::new("posts", "author");
        join.limit = Some(2);
        let field = FieldDefinition::builder("recent_posts", FieldType::Join)
            .join(join)
            .build();

        let mut jf = JoinField::default();
        enrich_join(&mut jf, &field, &ctx);

        let ids: Vec<&str> = jf
            .join_items
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|item| item.id.as_str())
            .collect();
        assert_eq!(ids, vec!["p6", "p7"]);
        assert_eq!(jf.join_total, None);
    }

    /// A count can say nothing the list may not: with the `on` field
    /// unreadable, no total is reported.
    #[test]
    fn no_total_is_reported_when_the_on_value_is_unreadable() {
        let jf = enriched(&registry(true), Some(1));

        assert_eq!(jf.join_total, None);
    }

    /// Regression: a join inside a group always rendered "no related items" —
    /// the nested enrichment (groups and their wrappers) had no join arm.
    #[test]
    fn a_join_in_a_group_lists_the_referencing_documents() {
        let reg = registry(false);
        let conn = posts_table();
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
            doc_id: Some("au1"),
            ancestor_readonly: false,
        };

        let defs = vec![
            FieldDefinition::builder("recent_posts", FieldType::Join)
                .join(JoinConfig::new("posts", "author"))
                .build(),
        ];
        let mut group_children = vec![FieldContext::Join(JoinField::default())];

        enrich_nested_fields(&mut group_children, &defs, &ctx);

        let FieldContext::Join(jf) = &group_children[0] else {
            panic!("join context");
        };
        assert_eq!(jf.join_count, Some(2));
    }
}
