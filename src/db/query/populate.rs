//! Relationship population (depth-based recursive loading).

use anyhow::Result;
use std::collections::HashSet;

use crate::core::{CollectionDefinition, Document};
use crate::core::field::FieldType;
use super::read::find_by_id;
use super::join::hydrate_document;

/// Convert a Document into a serde_json::Value for embedding in a parent's fields.
fn document_to_json(doc: &Document, collection: &str) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert("id".to_string(), serde_json::Value::String(doc.id.clone()));
    map.insert("collection".to_string(), serde_json::Value::String(collection.to_string()));
    for (k, v) in &doc.fields {
        map.insert(k.clone(), v.clone());
    }
    if let Some(ref ts) = doc.created_at {
        map.insert("created_at".to_string(), serde_json::Value::String(ts.clone()));
    }
    if let Some(ref ts) = doc.updated_at {
        map.insert("updated_at".to_string(), serde_json::Value::String(ts.clone()));
    }
    serde_json::Value::Object(map)
}

/// Recursively populate relationship fields with full document objects.
/// depth=0 is a no-op. Tracks visited (collection, id) pairs to break cycles.
/// If `select` is provided, only populate relationship fields in the select list.
/// Recursive calls for nested docs always pass `None` (populate all nested fields).
#[allow(clippy::too_many_arguments)]
pub fn populate_relationships(
    conn: &rusqlite::Connection,
    registry: &crate::core::Registry,
    collection_slug: &str,
    def: &CollectionDefinition,
    doc: &mut Document,
    depth: i32,
    visited: &mut HashSet<(String, String)>,
    select: Option<&[String]>,
) -> Result<()> {
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

        let rel_def = match registry.get_collection(&rel.collection) {
            Some(d) => d.clone(),
            None => continue,
        };

        if rel.has_many {
            // Has-many: doc.fields[name] is already a JSON array of ID strings (from hydration)
            let ids: Vec<String> = match doc.fields.get(&field.name) {
                Some(serde_json::Value::Array(arr)) => {
                    arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()
                }
                _ => continue,
            };

            let mut populated = Vec::new();
            for id in &ids {
                if visited.contains(&(rel.collection.clone(), id.clone())) {
                    // Already visited — keep as ID string
                    populated.push(serde_json::Value::String(id.clone()));
                    continue;
                }
                match find_by_id(conn, &rel.collection, &rel_def, id, None)? {
                    Some(mut related_doc) => {
                        hydrate_document(conn, &rel.collection, &rel_def, &mut related_doc, None, None)?;
                        if let Some(ref uc) = rel_def.upload {
                            if uc.enabled {
                                crate::core::upload::assemble_sizes_object(&mut related_doc, uc);
                            }
                        }
                        populate_relationships(
                            conn, registry, &rel.collection, &rel_def,
                            &mut related_doc, effective_depth - 1, visited, None,
                        )?;
                        populated.push(document_to_json(&related_doc, &rel.collection));
                    }
                    None => {
                        populated.push(serde_json::Value::String(id.clone()));
                    }
                }
            }
            doc.fields.insert(field.name.clone(), serde_json::Value::Array(populated));
        } else {
            // Has-one: doc.fields[name] is a string ID
            let id = match doc.fields.get(&field.name) {
                Some(serde_json::Value::String(s)) if !s.is_empty() => s.clone(),
                _ => continue,
            };

            if visited.contains(&(rel.collection.clone(), id.clone())) {
                continue; // Already visited — keep as ID string
            }

            if let Some(mut related_doc) = find_by_id(conn, &rel.collection, &rel_def, &id, None)? {
                hydrate_document(conn, &rel.collection, &rel_def, &mut related_doc, None, None)?;
                if let Some(ref uc) = rel_def.upload {
                    if uc.enabled {
                        crate::core::upload::assemble_sizes_object(&mut related_doc, uc);
                    }
                }
                populate_relationships(
                    conn, registry, &rel.collection, &rel_def,
                    &mut related_doc, effective_depth - 1, visited, None,
                )?;
                doc.fields.insert(field.name.clone(), document_to_json(&related_doc, &rel.collection));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use crate::core::{Document, Registry};
    use crate::core::collection::*;
    use crate::core::field::*;

    fn make_field(name: &str, ft: FieldType) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: ft,
            required: false,
            unique: false,
            validate: None,
            default_value: None,
            options: vec![],
            admin: FieldAdmin::default(),
            hooks: FieldHooks::default(),
            access: FieldAccess::default(),
            relationship: None,
            fields: vec![],
            blocks: vec![],
            localized: false,
        }
    }

    fn make_collection_def(slug: &str, fields: Vec<FieldDefinition>) -> CollectionDefinition {
        CollectionDefinition {
            slug: slug.to_string(),
            labels: CollectionLabels::default(),
            timestamps: true,
            fields,
            admin: CollectionAdmin::default(),
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            live: None,
            versions: None,
        }
    }

    // ── document_to_json tests ────────────────────────────────────────────────

    #[test]
    fn document_to_json_basic() {
        let mut doc = Document::new("doc1".to_string());
        doc.fields.insert("title".to_string(), serde_json::json!("Hello World"));
        doc.fields.insert("count".to_string(), serde_json::json!(42));
        doc.created_at = Some("2024-01-01T00:00:00Z".to_string());
        doc.updated_at = Some("2024-01-02T00:00:00Z".to_string());

        let json = document_to_json(&doc, "posts");
        let obj = json.as_object().expect("should be an object");

        assert_eq!(obj.get("id").and_then(|v| v.as_str()), Some("doc1"));
        assert_eq!(obj.get("collection").and_then(|v| v.as_str()), Some("posts"));
        assert_eq!(obj.get("title").and_then(|v| v.as_str()), Some("Hello World"));
        assert_eq!(obj.get("count").and_then(|v| v.as_i64()), Some(42));
        assert_eq!(obj.get("created_at").and_then(|v| v.as_str()), Some("2024-01-01T00:00:00Z"));
        assert_eq!(obj.get("updated_at").and_then(|v| v.as_str()), Some("2024-01-02T00:00:00Z"));
    }

    #[test]
    fn document_to_json_no_timestamps() {
        let mut doc = Document::new("doc2".to_string());
        doc.fields.insert("title".to_string(), serde_json::json!("No Timestamps"));
        // created_at and updated_at are None by default

        let json = document_to_json(&doc, "pages");
        let obj = json.as_object().expect("should be an object");

        assert_eq!(obj.get("id").and_then(|v| v.as_str()), Some("doc2"));
        assert_eq!(obj.get("collection").and_then(|v| v.as_str()), Some("pages"));
        assert_eq!(obj.get("title").and_then(|v| v.as_str()), Some("No Timestamps"));
        assert!(obj.get("created_at").is_none(), "created_at should be absent");
        assert!(obj.get("updated_at").is_none(), "updated_at should be absent");
    }

    #[test]
    fn document_to_json_with_nested() {
        let mut doc = Document::new("doc3".to_string());
        let nested = serde_json::json!({
            "meta": {
                "keywords": ["rust", "cms"],
                "score": 9.5
            }
        });
        doc.fields.insert("data".to_string(), nested.clone());

        let json = document_to_json(&doc, "entries");
        let obj = json.as_object().expect("should be an object");

        assert_eq!(obj.get("data"), Some(&nested));
        // Verify deep structure is preserved
        let data = obj.get("data").unwrap();
        let meta = data.get("meta").expect("meta should exist");
        assert_eq!(meta.get("score").and_then(|v| v.as_f64()), Some(9.5));
        let keywords = meta.get("keywords").and_then(|v| v.as_array()).expect("keywords should be array");
        assert_eq!(keywords.len(), 2);
        assert_eq!(keywords[0].as_str(), Some("rust"));
    }

    // ── populate_relationships tests ──────────────────────────────────────────

    fn setup_populate_db() -> Connection {
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
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO authors (id, name, created_at, updated_at)
                VALUES ('a1', 'Alice', '2024-01-01', '2024-01-01');
            INSERT INTO posts (id, title, author, created_at, updated_at)
                VALUES ('p1', 'Hello', 'a1', '2024-01-01', '2024-01-01');"
        ).unwrap();
        conn
    }

    fn make_authors_def() -> CollectionDefinition {
        make_collection_def("authors", vec![
            make_field("name", FieldType::Text),
        ])
    }

    fn make_posts_def() -> CollectionDefinition {
        let mut author_field = make_field("author", FieldType::Relationship);
        author_field.relationship = Some(RelationshipConfig {
            collection: "authors".to_string(),
            has_many: false,
            max_depth: None,
        });
        make_collection_def("posts", vec![
            make_field("title", FieldType::Text),
            author_field,
        ])
    }

    fn make_registry_with_posts_and_authors() -> Registry {
        let mut registry = Registry::new();
        registry.register_collection(make_posts_def());
        registry.register_collection(make_authors_def());
        registry
    }

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
        populate_relationships(
            &conn, &registry, "posts", &posts_def,
            &mut doc, 0, &mut visited, None,
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
        populate_relationships(
            &conn, &registry, "posts", &posts_def,
            &mut doc, 1, &mut visited, None,
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
        fav_post_field.relationship = Some(RelationshipConfig {
            collection: "posts".to_string(),
            has_many: false,
            max_depth: None,
        });
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
        let result = populate_relationships(
            &conn, &registry, "posts", &posts_def,
            &mut doc, 10, &mut visited, None,
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
}
