//! Join field (virtual reverse lookup) population.

use anyhow::Result;
use serde_json::Value;
use std::collections::HashSet;

use super::populate_relationships_cached;
use crate::core::cache::CacheBackend;
use crate::db::query::populate::{PopulateContext, PopulateOpts, document_to_json};
use crate::{
    core::{Document, FieldType, upload},
    db::{
        Filter, FilterClause, FilterOp, FindQuery,
        query::{hydrate_document, read::find},
    },
};

/// Populate join fields (virtual reverse lookups).
pub(super) fn populate_join_fields(
    ctx: &PopulateContext<'_>,
    doc: &mut Document,
    visited: &mut HashSet<(String, String)>,
    opts: &PopulateOpts<'_>,
    cache: &dyn CacheBackend,
) -> Result<()> {
    for field in &ctx.def.fields {
        if field.field_type != FieldType::Join {
            continue;
        }

        if let Some(sel) = opts.select
            && !sel.iter().any(|s| s == &field.name)
        {
            continue;
        }

        let jc = match &field.join {
            Some(jc) => jc,
            None => continue,
        };

        let target_def = match ctx.registry.get_collection(&jc.collection) {
            Some(d) => d.clone(),
            None => continue,
        };

        let populated = populate_join_docs(ctx, doc, jc, &target_def, visited, opts, cache)?;
        doc.fields
            .insert(field.name.clone(), Value::Array(populated));
    }

    Ok(())
}

/// Find, hydrate, and recursively populate matching documents for a join field.
fn populate_join_docs(
    ctx: &PopulateContext<'_>,
    doc: &Document,
    jc: &crate::core::field::JoinConfig,
    target_def: &crate::core::CollectionDefinition,
    visited: &mut HashSet<(String, String)>,
    opts: &PopulateOpts<'_>,
    cache: &dyn CacheBackend,
) -> Result<Vec<Value>> {
    let mut fq = FindQuery::new();

    fq.filters = vec![FilterClause::Single(Filter {
        field: jc.on.clone(),
        op: FilterOp::Equals(doc.id.to_string()),
    })];

    let matched_docs = match find(ctx.conn, &jc.collection, target_def, &fq, opts.locale_ctx) {
        Ok(docs) => docs,
        Err(_) => return Ok(Vec::new()),
    };

    let mut populated = Vec::new();

    for mut matched_doc in matched_docs {
        hydrate_document(
            ctx.conn,
            &jc.collection,
            &target_def.fields,
            &mut matched_doc,
            None,
            opts.locale_ctx,
        )?;

        if let Some(ref uc) = target_def.upload
            && uc.enabled
        {
            upload::assemble_sizes_object(&mut matched_doc, uc);
        }

        populate_relationships_cached(
            &PopulateContext {
                conn: ctx.conn,
                registry: ctx.registry,
                collection_slug: &jc.collection,
                def: target_def,
            },
            &mut matched_doc,
            visited,
            &PopulateOpts {
                depth: opts.depth - 1,
                select: None,
                locale_ctx: opts.locale_ctx,
            },
            cache,
        )?;

        populated.push(document_to_json(&matched_doc, &jc.collection));
    }

    Ok(populated)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::super::test_helpers::*;
    use super::super::super::{PopulateContext, PopulateOpts};
    use super::populate_relationships_cached;
    use crate::core::cache::NoneCache;
    use crate::core::{Document, Registry};
    use std::collections::HashSet;

    #[test]
    fn join_field_populates_reverse_docs() {
        let conn = setup_join_db();
        let authors_def = make_authors_def_with_join();
        let posts_def = make_posts_def_for_join();
        let mut registry = Registry::new();
        registry.register_collection(authors_def.clone());
        registry.register_collection(posts_def);

        let mut doc = Document::new("a1".to_string());
        doc.fields.insert("name".to_string(), json!("Alice"));

        let mut visited = HashSet::new();
        populate_relationships_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "authors",
                def: &authors_def,
            },
            &mut doc,
            &mut visited,
            &PopulateOpts {
                depth: 1,
                select: None,
                locale_ctx: None,
            },
            &NoneCache,
        )
        .unwrap();

        let posts = doc
            .fields
            .get("posts")
            .expect("posts join field should exist");
        let arr = posts.as_array().expect("posts should be an array");
        assert_eq!(arr.len(), 2, "Alice has 2 posts");

        let titles: Vec<&str> = arr
            .iter()
            .filter_map(|v| v.get("title").and_then(|t| t.as_str()))
            .collect();
        assert!(titles.contains(&"First Post"));
        assert!(titles.contains(&"Second Post"));
    }

    #[test]
    fn join_field_depth_zero_noop() {
        let conn = setup_join_db();
        let authors_def = make_authors_def_with_join();
        let posts_def = make_posts_def_for_join();
        let mut registry = Registry::new();
        registry.register_collection(authors_def.clone());
        registry.register_collection(posts_def);

        let mut doc = Document::new("a1".to_string());
        doc.fields.insert("name".to_string(), json!("Alice"));

        let mut visited = HashSet::new();
        populate_relationships_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "authors",
                def: &authors_def,
            },
            &mut doc,
            &mut visited,
            &PopulateOpts {
                depth: 0,
                select: None,
                locale_ctx: None,
            },
            &NoneCache,
        )
        .unwrap();

        // At depth=0, join field should not be populated
        assert!(
            !doc.fields.contains_key("posts"),
            "depth=0 should not add join field"
        );
    }

    #[test]
    fn join_field_no_matching_docs() {
        let conn = setup_join_db();
        let authors_def = make_authors_def_with_join();
        let posts_def = make_posts_def_for_join();
        let mut registry = Registry::new();
        registry.register_collection(authors_def.clone());
        registry.register_collection(posts_def);

        // Author with no posts
        let mut doc = Document::new("a99".to_string());
        doc.fields.insert("name".to_string(), json!("Nobody"));

        let mut visited = HashSet::new();
        populate_relationships_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "authors",
                def: &authors_def,
            },
            &mut doc,
            &mut visited,
            &PopulateOpts {
                depth: 1,
                select: None,
                locale_ctx: None,
            },
            &NoneCache,
        )
        .unwrap();

        let posts = doc
            .fields
            .get("posts")
            .expect("posts join field should exist");
        let arr = posts.as_array().expect("posts should be an array");
        assert!(
            arr.is_empty(),
            "no matching posts should produce empty array"
        );
    }

    #[test]
    fn join_field_select_filters() {
        let conn = setup_join_db();
        let authors_def = make_authors_def_with_join();
        let posts_def = make_posts_def_for_join();
        let mut registry = Registry::new();
        registry.register_collection(authors_def.clone());
        registry.register_collection(posts_def);

        let mut doc = Document::new("a1".to_string());
        doc.fields.insert("name".to_string(), json!("Alice"));

        let mut visited = HashSet::new();
        // Select only "name", not "posts"
        let select = vec!["name".to_string()];
        populate_relationships_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "authors",
                def: &authors_def,
            },
            &mut doc,
            &mut visited,
            &PopulateOpts {
                depth: 1,
                select: Some(&select),
                locale_ctx: None,
            },
            &NoneCache,
        )
        .unwrap();

        // Join field should be skipped because it's not in select
        assert!(
            !doc.fields.contains_key("posts"),
            "join field not in select should be skipped"
        );
    }
}
