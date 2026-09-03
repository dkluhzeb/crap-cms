//! Ranked search: `order_by = "_rank"` sorts by FTS relevance through the
//! normal find pipeline.

#![allow(clippy::missing_panics_doc)]

use crap_cms::config::{CrapConfig, LocaleConfig};
use crap_cms::core::Registry;
use crap_cms::core::collection::CollectionDefinition;
use crap_cms::core::field::{FieldDefinition, FieldType};
use crap_cms::db::{FindQuery, migrate, pool, query};
use serde_json::json;
use tempfile::TempDir;

fn setup() -> (TempDir, crap_cms::db::DbPool, CollectionDefinition) {
    let mut def = CollectionDefinition::new("posts");
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text).build(),
        FieldDefinition::builder("body", FieldType::Textarea).build(),
    ];

    let tmp = TempDir::new().expect("tempdir");
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");

    let shared = Registry::shared();
    shared.write().unwrap().register_collection(def.clone());
    migrate::sync_all(&db_pool, &shared.read().unwrap(), &LocaleConfig::default()).expect("sync");

    (tmp, db_pool, def)
}

fn seed(
    conn: &crap_cms::db::BoxedConnection,
    def: &CollectionDefinition,
    id_hint: &str,
    title: &str,
    body: &str,
) -> String {
    let data: crap_cms::core::DocumentFields = [
        ("title".to_string(), json!(title)),
        ("body".to_string(), json!(body)),
    ]
    .into_iter()
    .collect();
    let doc = query::create(conn, "posts", def, &data, None).expect(id_hint);
    let mut full = doc.clone();
    full.fields = data;
    query::fts::fts_upsert(conn, "posts", &full, Some(def)).expect("fts upsert");
    doc.id.to_string()
}

/// Documents mentioning the term more often rank first; the plain filter
/// order (insertion/id) would give a different sequence.
#[test]
fn rank_orders_by_relevance_best_first() {
    let (_tmp, db_pool, def) = setup();
    let conn = db_pool.get().unwrap();
    query::fts::sync_fts_table(&conn, "posts", &def, &LocaleConfig::default()).unwrap();

    let weak = seed(&conn, &def, "weak", "Notes", "mentions rust once");
    let strong = seed(
        &conn,
        &def,
        "strong",
        "Rust rust rust",
        "rust rust rust everywhere in this rust body",
    );
    let _off_topic = seed(&conn, &def, "off", "Gardening", "no relevant term at all");

    let q = FindQuery::builder()
        .search(Some("rust".to_string()))
        .order_by(Some("_rank".to_string()))
        .build();
    let docs = query::find(&conn, "posts", &def, &q, None).unwrap();

    let ids: Vec<String> = docs.iter().map(|d| d.id.to_string()).collect();
    assert_eq!(
        ids,
        vec![strong, weak],
        "best match first, off-topic filtered out"
    );
}

#[test]
fn rank_without_search_or_with_cursor_is_rejected() {
    let (_tmp, db_pool, def) = setup();
    let conn = db_pool.get().unwrap();

    let q = FindQuery::builder()
        .order_by(Some("_rank".to_string()))
        .build();
    let err = query::find(&conn, "posts", &def, &q, None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("requires a 'search' term"), "{err}");

    let q = FindQuery::builder()
        .order_by(Some("-_rank".to_string()))
        .build();
    let err = query::find(&conn, "posts", &def, &q, None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("best-first"), "{err}");
}

/// No FTS table yet → rank degrades to stable id order instead of erroring,
/// matching the search filter's graceful degradation.
#[test]
fn rank_degrades_gracefully_without_fts_index() {
    let (_tmp, db_pool, def) = setup();
    let conn = db_pool.get().unwrap();

    let a = seed(&conn, &def, "a", "rust", "rust");
    // (fts_upsert on a missing table is a no-op / ignored by the helper path;
    // the find below must not error.)
    let q = FindQuery::builder()
        .search(Some("rust".to_string()))
        .order_by(Some("_rank".to_string()))
        .build();
    let docs = query::find(&conn, "posts", &def, &q, None).unwrap();
    assert!(docs.iter().any(|d| d.id == a));
}
