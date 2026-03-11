//! Relationship population (depth-based recursive loading).

mod batch;
mod single;

pub use batch::populate_relationships_batch_cached;
pub use single::populate_relationships_cached;

use anyhow::Result;
use dashmap::DashMap;
use std::collections::HashSet;

use crate::core::{CollectionDefinition, Document};

/// Shared cache for populated documents. Key is (collection_slug, document_id).
/// Uses DashMap for concurrent cross-request sharing with interior mutability.
pub type PopulateCache = DashMap<(String, String), Document>;

/// Collection and registry context for population.
pub struct PopulateContext<'a> {
    pub conn: &'a rusqlite::Connection,
    pub registry: &'a crate::core::Registry,
    pub collection_slug: &'a str,
    pub def: &'a CollectionDefinition,
}

/// Options controlling population behavior.
pub struct PopulateOpts<'a> {
    pub depth: i32,
    pub select: Option<&'a [String]>,
    pub locale_ctx: Option<&'a super::LocaleContext>,
}

/// Parse a polymorphic reference "collection/id" into `(collection, id)`.
pub(crate) fn parse_poly_ref(s: &str) -> Option<(String, String)> {
    let pos = s.find('/')?;
    let col = &s[..pos];
    let id = &s[pos + 1..];
    if col.is_empty() || id.is_empty() {
        return None;
    }
    Some((col.to_string(), id.to_string()))
}

/// Convert a Document into a serde_json::Value for embedding in a parent's fields.
pub(crate) fn document_to_json(doc: &Document, collection: &str) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert("id".to_string(), serde_json::Value::String(doc.id.clone()));
    map.insert(
        "collection".to_string(),
        serde_json::Value::String(collection.to_string()),
    );
    for (k, v) in &doc.fields {
        map.insert(k.clone(), v.clone());
    }
    if let Some(ref ts) = doc.created_at {
        map.insert(
            "created_at".to_string(),
            serde_json::Value::String(ts.clone()),
        );
    }
    if let Some(ref ts) = doc.updated_at {
        map.insert(
            "updated_at".to_string(),
            serde_json::Value::String(ts.clone()),
        );
    }
    serde_json::Value::Object(map)
}

/// Recursively populate relationship fields with full document objects.
/// Convenience wrapper that creates a fresh cache per call.
pub fn populate_relationships(
    ctx: &PopulateContext<'_>,
    doc: &mut Document,
    visited: &mut HashSet<(String, String)>,
    opts: &PopulateOpts<'_>,
) -> Result<()> {
    let cache = PopulateCache::new();
    populate_relationships_cached(ctx, doc, visited, opts, &cache)
}

/// Batch-populate relationship fields across a slice of documents.
/// Convenience wrapper that creates a fresh cache per call.
pub fn populate_relationships_batch(
    ctx: &PopulateContext<'_>,
    docs: &mut [Document],
    opts: &PopulateOpts<'_>,
) -> Result<()> {
    let cache = PopulateCache::new();
    populate_relationships_batch_cached(ctx, docs, opts, &cache)
}

/// Shared test helpers used by single.rs and batch.rs test modules.
#[cfg(test)]
pub(crate) mod test_helpers {
    use crate::core::collection::*;
    use crate::core::field::*;
    use crate::core::Registry;
    use rusqlite::Connection;

    pub fn make_field(name: &str, ft: FieldType) -> FieldDefinition {
        FieldDefinition::builder(name, ft).build()
    }

    pub fn make_collection_def(slug: &str, fields: Vec<FieldDefinition>) -> CollectionDefinition {
        let mut def = CollectionDefinition::new(slug);
        def.fields = fields;
        def
    }

    pub fn setup_populate_db() -> Connection {
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
                VALUES ('p1', 'Hello', 'a1', '2024-01-01', '2024-01-01');",
        )
        .unwrap();
        conn
    }

    pub fn make_authors_def() -> CollectionDefinition {
        make_collection_def("authors", vec![make_field("name", FieldType::Text)])
    }

    pub fn make_posts_def() -> CollectionDefinition {
        let mut author_field = make_field("author", FieldType::Relationship);
        author_field.relationship = Some(RelationshipConfig::new("authors", false));
        make_collection_def(
            "posts",
            vec![make_field("title", FieldType::Text), author_field],
        )
    }

    pub fn make_registry_with_posts_and_authors() -> Registry {
        let mut registry = Registry::new();
        registry.register_collection(make_posts_def());
        registry.register_collection(make_authors_def());
        registry
    }

    pub fn setup_join_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE authors (
                id TEXT PRIMARY KEY,
                name TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                author TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO authors (id, name, created_at, updated_at)
                VALUES ('a1', 'Alice', '2024-01-01', '2024-01-01');
            INSERT INTO posts (id, title, author, created_at, updated_at)
                VALUES ('p1', 'First Post', 'a1', '2024-01-01', '2024-01-01');
            INSERT INTO posts (id, title, author, created_at, updated_at)
                VALUES ('p2', 'Second Post', 'a1', '2024-01-01', '2024-01-01');
            INSERT INTO posts (id, title, author, created_at, updated_at)
                VALUES ('p3', 'Other Post', 'a2', '2024-01-01', '2024-01-01');",
        )
        .unwrap();
        conn
    }

    pub fn make_authors_def_with_join() -> CollectionDefinition {
        let mut join_field = make_field("posts", FieldType::Join);
        join_field.join = Some(JoinConfig {
            collection: "posts".to_string(),
            on: "author".to_string(),
        });
        make_collection_def(
            "authors",
            vec![make_field("name", FieldType::Text), join_field],
        )
    }

    pub fn make_posts_def_for_join() -> CollectionDefinition {
        let mut author_field = make_field("author", FieldType::Relationship);
        author_field.relationship = Some(RelationshipConfig::new("authors", false));
        make_collection_def(
            "posts",
            vec![make_field("title", FieldType::Text), author_field],
        )
    }

    pub fn setup_polymorphic_populate_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE entries (
                id TEXT PRIMARY KEY,
                title TEXT,
                related TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            CREATE TABLE articles (
                id TEXT PRIMARY KEY,
                title TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            CREATE TABLE pages (
                id TEXT PRIMARY KEY,
                title TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            -- Polymorphic has-many junction table
            CREATE TABLE entries_refs (
                parent_id TEXT,
                related_id TEXT,
                related_collection TEXT NOT NULL DEFAULT '',
                _order INTEGER,
                PRIMARY KEY (parent_id, related_id, related_collection)
            );
            INSERT INTO articles VALUES ('a1', 'Article One', '2024-01-01', '2024-01-01');
            INSERT INTO pages VALUES ('pg1', 'Page One', '2024-01-01', '2024-01-01');
            INSERT INTO entries VALUES ('e1', 'Entry', 'articles/a1', '2024-01-01', '2024-01-01');",
        )
        .unwrap();
        conn
    }

    pub fn make_entries_def_poly_has_one() -> CollectionDefinition {
        let mut related_field = make_field("related", FieldType::Relationship);
        let mut rel = RelationshipConfig::new("articles", false);
        rel.polymorphic = vec!["articles".to_string(), "pages".to_string()];
        related_field.relationship = Some(rel);
        make_collection_def(
            "entries",
            vec![make_field("title", FieldType::Text), related_field],
        )
    }

    pub fn make_entries_def_poly_has_many() -> CollectionDefinition {
        let mut refs_field = make_field("refs", FieldType::Relationship);
        let mut rel = RelationshipConfig::new("articles", true);
        rel.polymorphic = vec!["articles".to_string(), "pages".to_string()];
        refs_field.relationship = Some(rel);
        make_collection_def(
            "entries",
            vec![make_field("title", FieldType::Text), refs_field],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── document_to_json tests ────────────────────────────────────────────────

    #[test]
    fn document_to_json_basic() {
        let mut doc = Document::new("doc1".to_string());
        doc.fields
            .insert("title".to_string(), serde_json::json!("Hello World"));
        doc.fields
            .insert("count".to_string(), serde_json::json!(42));
        doc.created_at = Some("2024-01-01T00:00:00Z".to_string());
        doc.updated_at = Some("2024-01-02T00:00:00Z".to_string());

        let json = document_to_json(&doc, "posts");
        let obj = json.as_object().expect("should be an object");

        assert_eq!(obj.get("id").and_then(|v| v.as_str()), Some("doc1"));
        assert_eq!(
            obj.get("collection").and_then(|v| v.as_str()),
            Some("posts")
        );
        assert_eq!(
            obj.get("title").and_then(|v| v.as_str()),
            Some("Hello World")
        );
        assert_eq!(obj.get("count").and_then(|v| v.as_i64()), Some(42));
        assert_eq!(
            obj.get("created_at").and_then(|v| v.as_str()),
            Some("2024-01-01T00:00:00Z")
        );
        assert_eq!(
            obj.get("updated_at").and_then(|v| v.as_str()),
            Some("2024-01-02T00:00:00Z")
        );
    }

    #[test]
    fn document_to_json_no_timestamps() {
        let mut doc = Document::new("doc2".to_string());
        doc.fields
            .insert("title".to_string(), serde_json::json!("No Timestamps"));
        // created_at and updated_at are None by default

        let json = document_to_json(&doc, "pages");
        let obj = json.as_object().expect("should be an object");

        assert_eq!(obj.get("id").and_then(|v| v.as_str()), Some("doc2"));
        assert_eq!(
            obj.get("collection").and_then(|v| v.as_str()),
            Some("pages")
        );
        assert_eq!(
            obj.get("title").and_then(|v| v.as_str()),
            Some("No Timestamps")
        );
        assert!(
            obj.get("created_at").is_none(),
            "created_at should be absent"
        );
        assert!(
            obj.get("updated_at").is_none(),
            "updated_at should be absent"
        );
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
        let keywords = meta
            .get("keywords")
            .and_then(|v| v.as_array())
            .expect("keywords should be array");
        assert_eq!(keywords.len(), 2);
        assert_eq!(keywords[0].as_str(), Some("rust"));
    }

    // ── parse_poly_ref tests ──────────────────────────────────────────────────

    #[test]
    fn parse_poly_ref_valid() {
        assert_eq!(
            parse_poly_ref("articles/a1"),
            Some(("articles".to_string(), "a1".to_string()))
        );
        assert_eq!(
            parse_poly_ref("a/b"),
            Some(("a".to_string(), "b".to_string()))
        );
    }

    #[test]
    fn parse_poly_ref_no_slash_returns_none() {
        assert_eq!(parse_poly_ref("noslash"), None);
        assert_eq!(parse_poly_ref(""), None);
    }

    #[test]
    fn parse_poly_ref_empty_col_returns_none() {
        // "/id" — collection portion is empty
        assert_eq!(parse_poly_ref("/someid"), None);
    }

    #[test]
    fn parse_poly_ref_empty_id_returns_none() {
        // "col/" — id portion is empty
        assert_eq!(parse_poly_ref("col/"), None);
    }
}
