//! Single-document relationship population (recursive, cached).

mod nonpoly;
mod poly;
mod join;

use anyhow::Result;
use std::collections::HashSet;

use crate::core::Document;
use crate::core::field::FieldType;
use super::{PopulateContext, PopulateOpts, PopulateCache};

/// Recursively populate relationship fields with full document objects.
/// depth=0 is a no-op. Tracks visited (collection, id) pairs to break cycles.
/// If `select` is provided, only populate relationship fields in the select list.
/// Uses a shared `cache` to avoid redundant fetches within the same request.
pub fn populate_relationships_cached(
    ctx: &PopulateContext<'_>,
    doc: &mut Document,
    visited: &mut HashSet<(String, String)>,
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
    if depth <= 0 {
        return Ok(());
    }

    let visit_key = (collection_slug.to_string(), doc.id.clone());
    if visited.contains(&visit_key) {
        return Ok(());
    }
    visited.insert(visit_key);

    for field in &def.fields {
        if field.field_type != FieldType::Relationship && field.field_type != FieldType::Upload {
            continue;
        }
        // Skip populating fields not in the select list
        if let Some(sel) = select {
            if !sel.iter().any(|s| s == &field.name) {
                continue;
            }
        }
        let rel = match &field.relationship {
            Some(rc) => rc,
            None => continue,
        };

        // Field-level max_depth caps the effective depth for this field
        let effective_depth = match rel.max_depth {
            Some(max) if max < depth => max,
            _ => depth,
        };
        if effective_depth <= 0 {
            continue;
        }

        if rel.is_polymorphic() {
            // Polymorphic: values are "collection/id" composite strings
            if rel.has_many {
                poly::populate_poly_has_many(conn, registry, doc, &field.name, visited, effective_depth, locale_ctx, cache)?;
            } else {
                poly::populate_poly_has_one(conn, registry, doc, &field.name, visited, effective_depth, locale_ctx, cache)?;
            }
        } else {
            // Non-polymorphic: look up the target collection definition
            let rel_def = match registry.get_collection(&rel.collection) {
                Some(d) => d.clone(),
                None => continue,
            };

            if rel.has_many {
                nonpoly::populate_nonpoly_has_many(conn, registry, doc, &field.name, &rel.collection, &rel_def, visited, effective_depth, locale_ctx, cache)?;
            } else {
                nonpoly::populate_nonpoly_has_one(conn, registry, doc, &field.name, &rel.collection, &rel_def, visited, effective_depth, locale_ctx, cache)?;
            }
        }
    }

    // Join fields: virtual reverse lookups
    join::populate_join_fields(ctx, doc, visited, opts, cache)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::test_helpers::*;
    use super::super::{PopulateContext, PopulateOpts, PopulateCache};
    use rusqlite::Connection;
    use crate::core::{Document, Registry};
    use crate::core::field::*;

    // ── populate_relationships (depth / basic hydration) ──────────────────────

    #[test]
    fn populate_depth_zero_noop() {
        let conn = setup_populate_db();
        let registry = make_registry_with_posts_and_authors();
        let posts_def = make_posts_def();

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert("title".to_string(), serde_json::json!("Hello"));
        doc.fields.insert("author".to_string(), serde_json::json!("a1"));
        doc.created_at = Some("2024-01-01".to_string());
        doc.updated_at = Some("2024-01-01".to_string());

        let mut visited = HashSet::new();
        populate_relationships_cached(
            &PopulateContext { conn: &conn, registry: &registry, collection_slug: "posts", def: &posts_def },
            &mut doc, &mut visited,
            &PopulateOpts { depth: 0, select: None, locale_ctx: None },
            &PopulateCache::new(),
        ).unwrap();

        // Author field should remain a string ID, not populated
        assert_eq!(
            doc.fields.get("author").and_then(|v| v.as_str()),
            Some("a1"),
            "depth=0 should not modify the document"
        );
    }

    #[test]
    fn populate_depth_one_hydrates() {
        let conn = setup_populate_db();
        let registry = make_registry_with_posts_and_authors();
        let posts_def = make_posts_def();

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert("title".to_string(), serde_json::json!("Hello"));
        doc.fields.insert("author".to_string(), serde_json::json!("a1"));
        doc.created_at = Some("2024-01-01".to_string());
        doc.updated_at = Some("2024-01-01".to_string());

        let mut visited = HashSet::new();
        populate_relationships_cached(
            &PopulateContext { conn: &conn, registry: &registry, collection_slug: "posts", def: &posts_def },
            &mut doc, &mut visited,
            &PopulateOpts { depth: 1, select: None, locale_ctx: None },
            &PopulateCache::new(),
        ).unwrap();

        // Author field should now be a populated object
        let author = doc.fields.get("author").expect("author field should exist");
        assert!(author.is_object(), "author should be populated as an object, got {:?}", author);

        let author_obj = author.as_object().unwrap();
        assert_eq!(author_obj.get("id").and_then(|v| v.as_str()), Some("a1"));
        assert_eq!(author_obj.get("name").and_then(|v| v.as_str()), Some("Alice"));
        assert_eq!(author_obj.get("collection").and_then(|v| v.as_str()), Some("authors"));
    }

    #[test]
    fn populate_circular_ref_stops() {
        // Set up two collections that reference each other: posts -> authors, authors -> posts
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                author TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            CREATE TABLE authors (
                id TEXT PRIMARY KEY,
                name TEXT,
                favorite_post TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO authors (id, name, favorite_post, created_at, updated_at)
                VALUES ('a1', 'Alice', 'p1', '2024-01-01', '2024-01-01');
            INSERT INTO posts (id, title, author, created_at, updated_at)
                VALUES ('p1', 'Hello', 'a1', '2024-01-01', '2024-01-01');"
        ).unwrap();

        // Authors def with a relationship back to posts
        let mut fav_post_field = make_field("favorite_post", FieldType::Relationship);
        fav_post_field.relationship = Some(RelationshipConfig::new("posts", false));
        let authors_def = make_collection_def("authors", vec![
            make_field("name", FieldType::Text),
            fav_post_field,
        ]);

        // Posts def with relationship to authors
        let posts_def = make_posts_def();

        let mut registry = Registry::new();
        registry.register_collection(posts_def.clone());
        registry.register_collection(authors_def);

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert("title".to_string(), serde_json::json!("Hello"));
        doc.fields.insert("author".to_string(), serde_json::json!("a1"));
        doc.created_at = Some("2024-01-01".to_string());
        doc.updated_at = Some("2024-01-01".to_string());

        // Pre-populate visited with the post itself to simulate already being in the chain
        let mut visited = HashSet::new();

        // Use high depth to ensure circular ref protection kicks in rather than depth limit
        let result = populate_relationships_cached(
            &PopulateContext { conn: &conn, registry: &registry, collection_slug: "posts", def: &posts_def },
            &mut doc, &mut visited,
            &PopulateOpts { depth: 10, select: None, locale_ctx: None },
            &PopulateCache::new(),
        );

        assert!(result.is_ok(), "should not infinite loop on circular references");

        // The author should be populated (first visit)
        let author = doc.fields.get("author").expect("author field should exist");
        assert!(author.is_object(), "author should be populated");

        // But the author's favorite_post should NOT be re-populated as a full object
        // because posts/p1 was already visited
        let author_obj = author.as_object().unwrap();
        let fav_post = author_obj.get("favorite_post");
        // It should either be the original string ID or absent (kept as-is due to visited check)
        if let Some(fp) = fav_post {
            assert!(
                fp.is_string(),
                "favorite_post should remain a string ID due to circular ref, got {:?}",
                fp
            );
        }
    }

    #[test]
    fn populate_field_level_max_depth_caps() {
        let conn = setup_populate_db();

        // Create a field with max_depth = 0 — should not populate even at depth=1
        let mut author_field = make_field("author", FieldType::Relationship);
        author_field.relationship = Some({
            let mut r = RelationshipConfig::new("authors", false);
            r.max_depth = Some(0);
            r
        });
        let posts_def = make_collection_def("posts", vec![
            make_field("title", FieldType::Text),
            author_field,
        ]);

        let mut registry = Registry::new();
        registry.register_collection(posts_def.clone());
        registry.register_collection(make_authors_def());

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert("title".to_string(), serde_json::json!("Hello"));
        doc.fields.insert("author".to_string(), serde_json::json!("a1"));

        let mut visited = HashSet::new();
        populate_relationships_cached(
            &PopulateContext { conn: &conn, registry: &registry, collection_slug: "posts", def: &posts_def },
            &mut doc, &mut visited,
            &PopulateOpts { depth: 1, select: None, locale_ctx: None },
            &PopulateCache::new(),
        ).unwrap();

        // author should remain a string ID because max_depth=0 caps effective_depth to 0
        assert_eq!(
            doc.fields.get("author").and_then(|v| v.as_str()),
            Some("a1"),
            "field-level max_depth=0 should prevent population"
        );
    }

    #[test]
    fn populate_select_filters_fields() {
        let conn = setup_populate_db();

        // Add a second relationship field
        let mut author_field = make_field("author", FieldType::Relationship);
        author_field.relationship = Some(RelationshipConfig::new("authors", false));
        let mut editor_field = make_field("editor", FieldType::Relationship);
        editor_field.relationship = Some(RelationshipConfig::new("authors", false));
        let posts_def = make_collection_def("posts", vec![
            make_field("title", FieldType::Text),
            author_field,
            editor_field,
        ]);

        let mut registry = Registry::new();
        registry.register_collection(posts_def.clone());
        registry.register_collection(make_authors_def());

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert("title".to_string(), serde_json::json!("Hello"));
        doc.fields.insert("author".to_string(), serde_json::json!("a1"));
        doc.fields.insert("editor".to_string(), serde_json::json!("a1"));

        let mut visited = HashSet::new();
        let select = vec!["author".to_string()]; // Only populate author, not editor
        populate_relationships_cached(
            &PopulateContext { conn: &conn, registry: &registry, collection_slug: "posts", def: &posts_def },
            &mut doc, &mut visited,
            &PopulateOpts { depth: 1, select: Some(&select), locale_ctx: None },
            &PopulateCache::new(),
        ).unwrap();

        // author should be populated
        let author = doc.fields.get("author").expect("author should exist");
        assert!(author.is_object(), "author should be populated");

        // editor should remain a string ID (not in select)
        assert_eq!(
            doc.fields.get("editor").and_then(|v| v.as_str()),
            Some("a1"),
            "editor should not be populated when not in select"
        );
    }

    #[test]
    fn populate_has_one_empty_string_skipped() {
        let conn = setup_populate_db();
        let registry = make_registry_with_posts_and_authors();
        let posts_def = make_posts_def();

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert("title".to_string(), serde_json::json!("Hello"));
        doc.fields.insert("author".to_string(), serde_json::json!(""));

        let mut visited = HashSet::new();
        populate_relationships_cached(
            &PopulateContext { conn: &conn, registry: &registry, collection_slug: "posts", def: &posts_def },
            &mut doc, &mut visited,
            &PopulateOpts { depth: 1, select: None, locale_ctx: None },
            &PopulateCache::new(),
        ).unwrap();

        // Empty string ID should be skipped (the `_ => continue` branch)
        assert_eq!(
            doc.fields.get("author").and_then(|v| v.as_str()),
            Some(""),
            "empty string author should not be populated"
        );
    }

    #[test]
    fn populate_relationships_wrapper_creates_fresh_cache() {
        // The wrapper creates a fresh PopulateCache per call — verify it works
        // by calling it twice and confirming it doesn't error (each call is independent)
        let conn = setup_populate_db();
        let registry = make_registry_with_posts_and_authors();
        let posts_def = make_posts_def();

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert("author".to_string(), serde_json::json!("a1"));

        let mut visited = HashSet::new();
        // First call — populates
        super::super::populate_relationships(
            &PopulateContext { conn: &conn, registry: &registry, collection_slug: "posts", def: &posts_def },
            &mut doc, &mut visited,
            &PopulateOpts { depth: 1, select: None, locale_ctx: None },
        ).unwrap();
        assert!(doc.fields["author"].is_object());

        // Reset and call again to confirm fresh cache (no stale state between calls)
        let mut doc2 = Document::new("p1".to_string());
        doc2.fields.insert("author".to_string(), serde_json::json!("a1"));
        let mut visited2 = HashSet::new();
        super::super::populate_relationships(
            &PopulateContext { conn: &conn, registry: &registry, collection_slug: "posts", def: &posts_def },
            &mut doc2, &mut visited2,
            &PopulateOpts { depth: 1, select: None, locale_ctx: None },
        ).unwrap();
        assert!(doc2.fields["author"].is_object());
        assert_eq!(doc2.fields["author"].get("name").and_then(|v| v.as_str()), Some("Alice"));
    }
}
