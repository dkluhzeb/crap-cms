//! Relationship population (depth-based recursive loading).

mod batch;
mod helpers;
mod single;
mod singleflight;
mod types;
mod wrappers;

pub use batch::{
    populate_relationships_batch_cached, populate_relationships_batch_cached_with_singleflight,
};
pub(crate) use helpers::{document_to_json, parse_poly_ref};
pub use single::{populate_relationships_cached, populate_relationships_cached_with_singleflight};
pub use singleflight::Singleflight;

/// Shared process-wide singleflight for deduplicating concurrent populate
/// cache misses across requests.
pub type SharedPopulateSingleflight =
    std::sync::Arc<singleflight::Singleflight<Option<crate::core::Document>>>;
pub use types::{JoinAccessCheck, PopulateContext, PopulateOpts, populate_cache_key};
pub(crate) use types::{PopulateCtx, locale_cache_key};
pub use wrappers::{populate_relationships, populate_relationships_batch};

/// Shared test helpers for populate tests — DB setup and collection definitions.
#[cfg(all(test, feature = "sqlite"))]
pub(crate) mod test_helpers {
    use crate::core::{Registry, Slug, collection::*, field::*};
    use crate::db::{DbConnection, InMemoryConn};

    // Re-export shared helpers so callers keep `use test_helpers::*`
    pub use crate::db::query::test_helpers::{make_field, make_group_field, make_tabs_field};

    pub fn make_collection_def(slug: &str, fields: Vec<FieldDefinition>) -> CollectionDefinition {
        crate::db::query::test_helpers::make_collection_def(slug, fields, false)
    }

    pub fn setup_populate_db() -> InMemoryConn {
        let conn = InMemoryConn::open();
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

    pub fn setup_join_db() -> InMemoryConn {
        let conn = InMemoryConn::open();
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
            collection: Slug::new("posts"),
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

    pub fn setup_polymorphic_populate_db() -> InMemoryConn {
        let conn = InMemoryConn::open();
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
        rel.polymorphic = vec!["articles".into(), "pages".into()];
        related_field.relationship = Some(rel);
        make_collection_def(
            "entries",
            vec![make_field("title", FieldType::Text), related_field],
        )
    }

    pub fn make_entries_def_poly_has_many() -> CollectionDefinition {
        let mut refs_field = make_field("refs", FieldType::Relationship);
        let mut rel = RelationshipConfig::new("articles", true);
        rel.polymorphic = vec!["articles".into(), "pages".into()];
        refs_field.relationship = Some(rel);
        make_collection_def(
            "entries",
            vec![make_field("title", FieldType::Text), refs_field],
        )
    }
}
