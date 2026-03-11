//! Batch relationship population across multiple documents.

mod nonpoly;
mod poly;

use anyhow::Result;
use std::collections::HashSet;

use super::single::populate_relationships_cached;
use super::{document_to_json, PopulateCache, PopulateContext, PopulateOpts};
use crate::core::field::FieldType;
use crate::core::Document;
use crate::db::query::{Filter, FilterClause, FilterOp, FindQuery};

/// Batch-populate relationship fields across a slice of documents.
///
/// Instead of calling `populate_relationships` per-document (N*M individual queries),
/// this collects all referenced IDs across all documents per field, batch-fetches them
/// with a single `find_by_ids` query per target collection, then distributes the results
/// back. Uses a shared `cache` to avoid redundant fetches within the same request.
///
/// This is the hot path for `Find` with `depth >= 1` — it turns O(N*M) queries into
/// O(M) queries where M is the number of relationship fields.
pub fn populate_relationships_batch_cached(
    ctx: &PopulateContext<'_>,
    docs: &mut [Document],
    opts: &PopulateOpts<'_>,
    cache: &PopulateCache,
) -> Result<()> {
    let conn = ctx.conn;
    let registry = ctx.registry;
    let collection_slug = ctx.collection_slug;
    let def = ctx.def;
    let depth = opts.depth;
    let select = opts.select;
    let locale_ctx = opts.locale_ctx;
    if depth <= 0 || docs.is_empty() {
        return Ok(());
    }

    // Shared visited set across all documents for cross-document dedup
    let mut visited: HashSet<(String, String)> = HashSet::new();
    // Mark all parent documents as visited to prevent circular population
    for doc in docs.iter() {
        visited.insert((collection_slug.to_string(), doc.id.clone()));
    }

    // -- Non-join relationship/upload fields --
    for field in &def.fields {
        if field.field_type != FieldType::Relationship && field.field_type != FieldType::Upload {
            continue;
        }
        if let Some(sel) = select {
            if !sel.iter().any(|s| s == &field.name) {
                continue;
            }
        }
        let rel = match &field.relationship {
            Some(rc) => rc,
            None => continue,
        };

        let effective_depth = match rel.max_depth {
            Some(max) if max < depth => max,
            _ => depth,
        };
        if effective_depth <= 0 {
            continue;
        }

        if rel.is_polymorphic() {
            if rel.has_many {
                poly::batch_poly_has_many(
                    conn,
                    registry,
                    docs,
                    &field.name,
                    &visited,
                    effective_depth,
                    locale_ctx,
                    cache,
                )?;
            } else {
                poly::batch_poly_has_one(
                    conn,
                    registry,
                    docs,
                    &field.name,
                    &visited,
                    effective_depth,
                    locale_ctx,
                    cache,
                )?;
            }
        } else {
            let rel_def = match registry.get_collection(&rel.collection) {
                Some(d) => d.clone(),
                None => continue,
            };

            if rel.has_many {
                nonpoly::batch_nonpoly_has_many(
                    conn,
                    registry,
                    docs,
                    &field.name,
                    &rel.collection,
                    &rel_def,
                    &visited,
                    effective_depth,
                    locale_ctx,
                    cache,
                )?;
            } else {
                nonpoly::batch_nonpoly_has_one(
                    conn,
                    registry,
                    docs,
                    &field.name,
                    &rel.collection,
                    &rel_def,
                    &visited,
                    effective_depth,
                    locale_ctx,
                    cache,
                )?;
            }
        }
    }

    // -- Join fields: fall through to per-doc (reverse lookups can't batch easily) --
    let has_join_fields = def.fields.iter().any(|f| {
        f.field_type == FieldType::Join
            && f.join.is_some()
            && select.is_none_or(|sel| sel.iter().any(|s| s == &f.name))
    });
    if has_join_fields {
        for doc in docs.iter_mut() {
            let mut doc_visited = visited.clone();
            for field in &def.fields {
                if field.field_type != FieldType::Join {
                    continue;
                }
                if let Some(sel) = select {
                    if !sel.iter().any(|s| s == &field.name) {
                        continue;
                    }
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
                if let Ok(matched_docs) =
                    crate::db::query::read::find(conn, &jc.collection, &target_def, &fq, locale_ctx)
                {
                    let mut populated = Vec::new();
                    for mut matched_doc in matched_docs {
                        crate::db::query::hydrate_document(
                            conn,
                            &jc.collection,
                            &target_def.fields,
                            &mut matched_doc,
                            None,
                            locale_ctx,
                        )?;
                        if let Some(ref uc) = target_def.upload {
                            if uc.enabled {
                                crate::core::upload::assemble_sizes_object(&mut matched_doc, uc);
                            }
                        }
                        populate_relationships_cached(
                            &PopulateContext {
                                conn,
                                registry,
                                collection_slug: &jc.collection,
                                def: &target_def,
                            },
                            &mut matched_doc,
                            &mut doc_visited,
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
                        .insert(field.name.clone(), serde_json::Value::Array(populated));
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::*;
    use super::super::{PopulateCache, PopulateContext, PopulateOpts};
    use super::*;
    use crate::core::field::*;
    use crate::core::{Document, Registry};
    use rusqlite::Connection;

    // ── Basic depth/empty guard ───────────────────────────────────────────────

    #[test]
    fn batch_depth_zero_noop() {
        let conn = setup_populate_db();
        let registry = make_registry_with_posts_and_authors();
        let posts_def = make_posts_def();

        let mut docs = vec![];
        populate_relationships_batch_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "posts",
                def: &posts_def,
            },
            &mut docs,
            &PopulateOpts {
                depth: 0,
                select: None,
                locale_ctx: None,
            },
            &PopulateCache::new(),
        )
        .unwrap();
        // Empty docs + depth 0 → no-op, no error
    }

    #[test]
    fn batch_empty_docs_noop() {
        let conn = setup_populate_db();
        let registry = make_registry_with_posts_and_authors();
        let posts_def = make_posts_def();

        let mut docs = vec![];
        populate_relationships_batch_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "posts",
                def: &posts_def,
            },
            &mut docs,
            &PopulateOpts {
                depth: 1,
                select: None,
                locale_ctx: None,
            },
            &PopulateCache::new(),
        )
        .unwrap();
    }

    // ── Select filtering ──────────────────────────────────────────────────────

    #[test]
    fn batch_select_filters_fields() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY, title TEXT, author TEXT, editor TEXT, created_at TEXT, updated_at TEXT);
             CREATE TABLE authors (id TEXT PRIMARY KEY, name TEXT, created_at TEXT, updated_at TEXT);
             INSERT INTO authors VALUES ('a1', 'Alice', '2024-01-01', '2024-01-01');
             INSERT INTO posts VALUES ('p1', 'Post 1', 'a1', 'a1', '2024-01-01', '2024-01-01');"
        ).unwrap();

        let mut author_field = make_field("author", FieldType::Relationship);
        author_field.relationship = Some(RelationshipConfig::new("authors", false));
        let mut editor_field = make_field("editor", FieldType::Relationship);
        editor_field.relationship = Some(RelationshipConfig::new("authors", false));
        let posts_def = make_collection_def(
            "posts",
            vec![
                make_field("title", FieldType::Text),
                author_field,
                editor_field,
            ],
        );
        let mut registry = Registry::new();
        registry.register_collection(posts_def.clone());
        registry.register_collection(make_authors_def());

        let mut docs = vec![{
            let mut d = Document::new("p1".to_string());
            d.fields
                .insert("author".to_string(), serde_json::json!("a1"));
            d.fields
                .insert("editor".to_string(), serde_json::json!("a1"));
            d
        }];

        let select = vec!["author".to_string()];
        populate_relationships_batch_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "posts",
                def: &posts_def,
            },
            &mut docs,
            &PopulateOpts {
                depth: 1,
                select: Some(&select),
                locale_ctx: None,
            },
            &PopulateCache::new(),
        )
        .unwrap();

        // author should be populated
        assert!(docs[0].fields["author"].is_object());
        // editor should remain as ID (not in select)
        assert_eq!(docs[0].fields["editor"].as_str(), Some("a1"));
    }

    // ── Field-level max_depth ─────────────────────────────────────────────────

    #[test]
    fn batch_max_depth_zero_stays_as_id() {
        let conn = setup_populate_db();

        let mut author_field = make_field("author", FieldType::Relationship);
        let mut rel = RelationshipConfig::new("authors", false);
        rel.max_depth = Some(0);
        author_field.relationship = Some(rel);
        let posts_def = make_collection_def(
            "posts",
            vec![make_field("title", FieldType::Text), author_field],
        );
        let mut registry = Registry::new();
        registry.register_collection(posts_def.clone());
        registry.register_collection(make_authors_def());

        let mut docs = vec![{
            let mut d = Document::new("p1".to_string());
            d.fields
                .insert("author".to_string(), serde_json::json!("a1"));
            d
        }];

        populate_relationships_batch_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "posts",
                def: &posts_def,
            },
            &mut docs,
            &PopulateOpts {
                depth: 1,
                select: None,
                locale_ctx: None,
            },
            &PopulateCache::new(),
        )
        .unwrap();

        // max_depth=0 should prevent population
        assert_eq!(docs[0].fields["author"].as_str(), Some("a1"));
    }

    // ── Missing related docs ──────────────────────────────────────────────────

    #[test]
    fn batch_missing_related_docs_stay_as_ids() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY, title TEXT, author TEXT, created_at TEXT, updated_at TEXT);
             CREATE TABLE authors (id TEXT PRIMARY KEY, name TEXT, created_at TEXT, updated_at TEXT);
             INSERT INTO posts VALUES ('p1', 'Post 1', 'nonexistent', '2024-01-01', '2024-01-01');"
        ).unwrap();

        let registry = make_registry_with_posts_and_authors();
        let posts_def = make_posts_def();

        let mut docs = vec![{
            let mut d = Document::new("p1".to_string());
            d.fields
                .insert("author".to_string(), serde_json::json!("nonexistent"));
            d
        }];

        populate_relationships_batch_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "posts",
                def: &posts_def,
            },
            &mut docs,
            &PopulateOpts {
                depth: 1,
                select: None,
                locale_ctx: None,
            },
            &PopulateCache::new(),
        )
        .unwrap();

        // Missing doc stays as ID string
        assert_eq!(docs[0].fields["author"].as_str(), Some("nonexistent"));
    }

    // ── Join fields in batch ──────────────────────────────────────────────────

    #[test]
    fn batch_with_join_field() {
        let conn = setup_join_db();
        let authors_def = make_authors_def_with_join();
        let posts_def = make_posts_def_for_join();
        let mut registry = Registry::new();
        registry.register_collection(authors_def.clone());
        registry.register_collection(posts_def);

        let mut docs = vec![{
            let mut d = Document::new("a1".to_string());
            d.fields
                .insert("name".to_string(), serde_json::json!("Alice"));
            d
        }];

        populate_relationships_batch_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "authors",
                def: &authors_def,
            },
            &mut docs,
            &PopulateOpts {
                depth: 1,
                select: None,
                locale_ctx: None,
            },
            &PopulateCache::new(),
        )
        .unwrap();

        let posts = docs[0]
            .fields
            .get("posts")
            .expect("join field should be populated");
        let arr = posts.as_array().unwrap();
        assert_eq!(arr.len(), 2, "Alice has 2 posts");
    }

    // ── populate_relationships_batch wrapper ──────────────────────────────────

    #[test]
    fn populate_relationships_batch_wrapper_creates_fresh_cache() {
        let conn = setup_populate_db();
        let registry = make_registry_with_posts_and_authors();
        let posts_def = make_posts_def();

        let mut docs = vec![{
            let mut d = Document::new("p1".to_string());
            d.fields
                .insert("author".to_string(), serde_json::json!("a1"));
            d
        }];

        // wrapper should succeed (creates fresh cache internally)
        super::super::populate_relationships_batch(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "posts",
                def: &posts_def,
            },
            &mut docs,
            &PopulateOpts {
                depth: 1,
                select: None,
                locale_ctx: None,
            },
        )
        .unwrap();
        assert!(docs[0].fields["author"].is_object());
        assert_eq!(
            docs[0].fields["author"]
                .get("name")
                .and_then(|v| v.as_str()),
            Some("Alice")
        );
    }
}
