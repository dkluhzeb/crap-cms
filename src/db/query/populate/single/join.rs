//! Join field (virtual reverse lookup) population.

use anyhow::Result;
use serde_json::Value;
use std::collections::HashSet;

use super::{
    super::{PopulateCache, PopulateContext, PopulateOpts, document_to_json},
    populate_relationships_cached,
};
use crate::{
    core::{Document, field::FieldType, upload},
    db::query::{Filter, FilterClause, FilterOp, FindQuery, hydrate_document, read::find},
};

/// Populate join fields (virtual reverse lookups).
pub(super) fn populate_join_fields(
    ctx: &PopulateContext<'_>,
    doc: &mut Document,
    visited: &mut HashSet<(String, String)>,
    opts: &PopulateOpts<'_>,
    cache: &PopulateCache,
) -> Result<()> {
    let conn = ctx.conn;
    let registry = ctx.registry;
    let depth = opts.depth;
    let select = opts.select;
    let locale_ctx = opts.locale_ctx;

    for field in &ctx.def.fields {
        if field.field_type != FieldType::Join {
            continue;
        }
        if let Some(sel) = select
            && !sel.iter().any(|s| s == &field.name)
        {
            continue;
        }
        let jc = match &field.join {
            Some(jc) => jc,
            None => continue,
        };
        let target_def = match registry.get_collection(&jc.collection) {
            Some(d) => d.clone(),
            None => continue,
        };
        let mut fq = FindQuery::new();
        fq.filters = vec![FilterClause::Single(Filter {
            field: jc.on.clone(),
            op: FilterOp::Equals(doc.id.clone()),
        })];
        let fq = fq;

        if let Ok(matched_docs) = find(conn, &jc.collection, &target_def, &fq, locale_ctx) {
            let mut populated = Vec::new();
            for mut matched_doc in matched_docs {
                hydrate_document(
                    conn,
                    &jc.collection,
                    &target_def.fields,
                    &mut matched_doc,
                    None,
                    locale_ctx,
                )?;

                if let Some(ref uc) = target_def.upload
                    && uc.enabled
                {
                    upload::assemble_sizes_object(&mut matched_doc, uc);
                }
                populate_relationships_cached(
                    &PopulateContext {
                        conn,
                        registry,
                        collection_slug: &jc.collection,
                        def: &target_def,
                    },
                    &mut matched_doc,
                    visited,
                    &PopulateOpts {
                        depth: depth - 1,
                        select: None,
                        locale_ctx,
                    },
                    cache,
                )?;
                populated.push(document_to_json(&matched_doc, &jc.collection));
            }
            doc.fields
                .insert(field.name.clone(), Value::Array(populated));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::super::test_helpers::*;
    use super::super::super::{PopulateCache, PopulateContext, PopulateOpts};
    use super::populate_relationships_cached;
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
            &PopulateCache::new(),
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
            &PopulateCache::new(),
        )
        .unwrap();

        // At depth=0, join field should not be populated
        assert!(
            doc.fields.get("posts").is_none(),
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
            &PopulateCache::new(),
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
            &PopulateCache::new(),
        )
        .unwrap();

        // Join field should be skipped because it's not in select
        assert!(
            doc.fields.get("posts").is_none(),
            "join field not in select should be skipped"
        );
    }
}
