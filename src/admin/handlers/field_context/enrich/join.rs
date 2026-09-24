//! Join field enrichment: the reverse-lookup items a join lists.

use crate::{
    admin::{
        context::field::{JoinField, JoinItem},
        handlers::field_context::enrich::{EnrichCtx, gated_find},
    },
    core::{Document, FieldDefinition},
    db::{Filter, FilterClause, FilterOp},
    service::join_child_readable,
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

/// Enrich a top-level Join field context with reverse-lookup items from DB.
///
/// Access-gated so the join never enumerates, labels, or counts rows the
/// viewer cannot read — nor rows whose `on` value the viewer cannot read,
/// since a row's presence in the join is that value.
pub(super) fn enrich_join(
    jf: &mut JoinField,
    field_def: &FieldDefinition,
    ctx: &EnrichCtx,
    doc_id: Option<&str>,
) {
    let Some(jc) = &field_def.join else {
        return;
    };
    let Some(doc_id) = doc_id else {
        return;
    };
    let Some(target_def) = ctx.reg.get_collection(&jc.collection) else {
        return;
    };

    let base = vec![FilterClause::Single(Filter {
        field: jc.on.clone(),
        op: FilterOp::Equals(doc_id.to_string()),
    })];

    // `gated_find` strips what the viewer may not read; a child whose `on`
    // value went with it is neither listed nor counted.
    let mut docs = gated_find(ctx, &jc.collection, target_def, base);
    docs.retain(|doc| join_child_readable(jc, &doc.fields));

    let items: Vec<JoinItem> = docs
        .iter()
        .map(|doc| join_item(doc, target_def.title_field()))
        .collect();

    jf.join_count = Some(items.len());
    jf.join_items = Some(items);
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::collections::HashMap;

    use rusqlite::Connection;

    use super::*;
    use crate::{
        admin::handlers::field_context::enrich::test_helpers::make_test_state_with_deny,
        core::{CollectionDefinition, FieldType, JoinConfig, Registry, RelationshipConfig},
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
                id TEXT PRIMARY KEY, title TEXT, author TEXT,
                _status TEXT DEFAULT 'published', created_at TEXT, updated_at TEXT
            );
            INSERT INTO posts (id, title, author) VALUES ('p1', 'One', 'au1');
            INSERT INTO posts (id, title, author) VALUES ('p2', 'Two', 'au1');",
        )
        .unwrap();

        conn
    }

    /// Enrich `authors.recent_posts` (joining `posts` on `author`) for `au1`.
    fn enriched(reg: &Registry) -> JoinField {
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
            ancestor_readonly: false,
        };

        let field = FieldDefinition::builder("recent_posts", FieldType::Join)
            .join(JoinConfig::new("posts", "author"))
            .build();

        let mut jf = JoinField::default();
        enrich_join(&mut jf, &field, &ctx, Some("au1"));

        jf
    }

    #[test]
    fn a_join_lists_and_counts_the_documents_that_reference_this_one() {
        let jf = enriched(&registry(false));

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
        let jf = enriched(&registry(true));

        assert_eq!(jf.join_items.as_deref().map(<[JoinItem]>::len), Some(0));
        assert_eq!(jf.join_count, Some(0));
    }
}
