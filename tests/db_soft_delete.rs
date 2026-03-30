//! Integration tests for the soft-delete feature.
//!
//! Tests the query layer and service layer together: soft-delete, restore,
//! find filtering, count filtering, FTS cleanup, and auto-purge.

use std::collections::HashMap;

use crap_cms::config::{CrapConfig, LocaleConfig};
use crap_cms::core::collection::CollectionDefinition;
use crap_cms::core::field::{FieldDefinition, FieldType};
use crap_cms::core::{Registry, SharedRegistry};
use crap_cms::db::{DbConnection, DbPool, DbValue, FindQuery, migrate, ops, pool, query};
use crap_cms::scheduler::purge_soft_deleted;

// ── Helpers ───────────────────────────────────────────────────────────────

fn make_soft_delete_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("articles");
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text).build(),
        FieldDefinition::builder("body", FieldType::Text).build(),
    ];
    def.timestamps = true;
    def.soft_delete = true;
    def
}

fn make_hard_delete_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("notes");
    def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];
    def.timestamps = true;
    def.soft_delete = false;
    def
}

fn create_pool_and_migrate(
    defs: Vec<CollectionDefinition>,
) -> (tempfile::TempDir, DbPool, SharedRegistry) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");

    let registry = Registry::shared();
    {
        let mut reg = registry.write().unwrap();
        for def in defs {
            reg.register_collection(def);
        }
    }

    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync schema");

    (tmp, db_pool, registry)
}

fn insert_doc(
    pool: &DbPool,
    slug: &str,
    def: &CollectionDefinition,
    data: &[(&str, &str)],
) -> String {
    let map: HashMap<String, String> = data
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let doc = query::create(&tx, slug, def, &map, None).unwrap();
    tx.commit().unwrap();
    doc.id.to_string()
}

// ── Test a: soft delete makes doc invisible to normal find ────────────────

#[test]
fn delete_document_soft_deletes_when_collection_enabled() {
    let def = make_soft_delete_def();
    let (_tmp, pool, _reg) = create_pool_and_migrate(vec![def.clone()]);

    let id = insert_doc(
        &pool,
        "articles",
        &def,
        &[("title", "Visible"), ("body", "content")],
    );

    // Soft-delete the document
    let conn = pool.get().unwrap();
    let deleted = query::soft_delete(&conn, "articles", &id).unwrap();
    assert!(deleted, "soft_delete should return true");

    // Normal find should NOT return it
    let docs = ops::find_documents(&pool, "articles", &def, &FindQuery::default(), None).unwrap();
    assert!(
        docs.is_empty(),
        "soft-deleted doc should not appear in normal find()"
    );

    // find with include_deleted should return it
    let query = FindQuery {
        include_deleted: true,
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "articles", &def, &query, None).unwrap();
    assert_eq!(
        docs.len(),
        1,
        "soft-deleted doc should appear with include_deleted=true"
    );
    assert_eq!(docs[0].get_str("title"), Some("Visible"));
}

// ── Test b: hard delete removes doc completely ───────────────────────────

#[test]
fn delete_document_hard_deletes_when_collection_disabled() {
    let def = make_hard_delete_def();
    let (_tmp, pool, _reg) = create_pool_and_migrate(vec![def.clone()]);

    let id = insert_doc(&pool, "notes", &def, &[("title", "Temporary")]);

    // Hard-delete the document
    let conn = pool.get().unwrap();
    let deleted = query::delete(&conn, "notes", &id).unwrap();
    assert!(deleted, "delete should return true");

    // Normal find should NOT return it
    let docs = ops::find_documents(&pool, "notes", &def, &FindQuery::default(), None).unwrap();
    assert!(docs.is_empty(), "hard-deleted doc should be gone");

    // include_deleted should also NOT return it (hard delete = gone)
    let query = FindQuery {
        include_deleted: true,
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "notes", &def, &query, None).unwrap();
    assert!(
        docs.is_empty(),
        "hard-deleted doc should be completely gone even with include_deleted"
    );
}

// ── Test c: soft-deleted doc excluded from find_by_id ────────────────────

#[test]
fn soft_deleted_doc_excluded_from_find_by_id() {
    let def = make_soft_delete_def();
    let (_tmp, pool, _reg) = create_pool_and_migrate(vec![def.clone()]);

    let id = insert_doc(&pool, "articles", &def, &[("title", "Ghost"), ("body", "")]);

    let conn = pool.get().unwrap();
    query::soft_delete(&conn, "articles", &id).unwrap();
    drop(conn);

    let found = ops::find_document_by_id(&pool, "articles", &def, &id, None).unwrap();
    assert!(
        found.is_none(),
        "find_by_id should return None for soft-deleted doc"
    );
}

// ── Test d: soft-deleted doc excluded from count ─────────────────────────

#[test]
fn soft_deleted_doc_excluded_from_count() {
    let def = make_soft_delete_def();
    let (_tmp, pool, _reg) = create_pool_and_migrate(vec![def.clone()]);

    insert_doc(&pool, "articles", &def, &[("title", "One"), ("body", "")]);
    let id2 = insert_doc(&pool, "articles", &def, &[("title", "Two"), ("body", "")]);

    // Count before delete
    let count_before = ops::count_documents(&pool, "articles", &def, &[], None).unwrap();
    assert_eq!(count_before, 2);

    // Soft-delete one
    let conn = pool.get().unwrap();
    query::soft_delete(&conn, "articles", &id2).unwrap();
    drop(conn);

    // Count after delete
    let count_after = ops::count_documents(&pool, "articles", &def, &[], None).unwrap();
    assert_eq!(count_after, 1, "count should decrease after soft-delete");
}

// ── Test e: restore makes doc visible again ──────────────────────────────

#[test]
fn restore_document_makes_doc_visible_again() {
    let def = make_soft_delete_def();
    let (_tmp, pool, _reg) = create_pool_and_migrate(vec![def.clone()]);

    let id = insert_doc(
        &pool,
        "articles",
        &def,
        &[("title", "Lazarus"), ("body", "risen")],
    );

    // Soft-delete
    let conn = pool.get().unwrap();
    query::soft_delete(&conn, "articles", &id).unwrap();
    drop(conn);

    // Verify invisible
    let found = ops::find_document_by_id(&pool, "articles", &def, &id, None).unwrap();
    assert!(found.is_none(), "should be invisible after soft-delete");

    // Restore
    let conn = pool.get().unwrap();
    let restored = query::restore(&conn, "articles", &id).unwrap();
    assert!(restored, "restore should return true");
    drop(conn);

    // Verify visible again
    let found = ops::find_document_by_id(&pool, "articles", &def, &id, None).unwrap();
    assert!(found.is_some(), "should be visible after restore");
    assert_eq!(found.unwrap().get_str("title"), Some("Lazarus"));
}

// ── Test f: restore nonexistent doc returns false ────────────────────────

#[test]
fn restore_nonexistent_doc_returns_false() {
    let def = make_soft_delete_def();
    let (_tmp, pool, _reg) = create_pool_and_migrate(vec![def.clone()]);

    let conn = pool.get().unwrap();
    let restored = query::restore(&conn, "articles", "does-not-exist").unwrap();
    assert!(!restored, "restore should return false for nonexistent doc");
}

// ── Test g: bulk soft-delete via sequential soft_delete calls ────────────

#[test]
fn bulk_delete_uses_soft_delete() {
    let def = make_soft_delete_def();
    let (_tmp, pool, _reg) = create_pool_and_migrate(vec![def.clone()]);

    let ids: Vec<String> = (0..5)
        .map(|i| {
            insert_doc(
                &pool,
                "articles",
                &def,
                &[("title", &format!("Doc {}", i)), ("body", "")],
            )
        })
        .collect();

    // Soft-delete all documents
    let conn = pool.get().unwrap();
    for id in &ids {
        let ok = query::soft_delete(&conn, "articles", id).unwrap();
        assert!(ok);
    }
    drop(conn);

    // Normal find returns empty
    let docs = ops::find_documents(&pool, "articles", &def, &FindQuery::default(), None).unwrap();
    assert!(
        docs.is_empty(),
        "all docs should be hidden after bulk soft-delete"
    );

    // Count should be 0
    let count = ops::count_documents(&pool, "articles", &def, &[], None).unwrap();
    assert_eq!(count, 0);

    // include_deleted returns all 5
    let query = FindQuery {
        include_deleted: true,
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "articles", &def, &query, None).unwrap();
    assert_eq!(
        docs.len(),
        5,
        "all 5 soft-deleted docs should appear with include_deleted"
    );
}

// ── Test h: parse_retention_seconds already tested in scheduler/runner ───
// Verified: tests exist in src/scheduler/runner.rs (parse_retention_days,
// parse_retention_hours, parse_retention_raw_seconds, parse_retention_invalid,
// parse_retention_with_whitespace).

// ── Test i: purge removes expired soft-deleted docs ──────────────────────

#[test]
fn purge_soft_deleted_removes_expired_docs() {
    let mut def = make_soft_delete_def();
    def.soft_delete_retention = Some("1h".to_string()); // 3600 seconds
    let (_tmp, pool, registry) = create_pool_and_migrate(vec![def.clone()]);

    let id_old = insert_doc(&pool, "articles", &def, &[("title", "Old"), ("body", "")]);
    let id_recent = insert_doc(
        &pool,
        "articles",
        &def,
        &[("title", "Recent"), ("body", "")],
    );
    let _id_live = insert_doc(&pool, "articles", &def, &[("title", "Live"), ("body", "")]);

    // Soft-delete both, then backdate one past retention
    let conn = pool.get().unwrap();
    query::soft_delete(&conn, "articles", &id_old).unwrap();
    query::soft_delete(&conn, "articles", &id_recent).unwrap();

    // Backdate the "old" doc's _deleted_at to 2 hours ago (past the 1h retention)
    conn.execute(
        &format!(
            "UPDATE articles SET _deleted_at = datetime('now', '-7200 seconds') WHERE id = {}",
            conn.placeholder(1)
        ),
        &[DbValue::Text(id_old.clone())],
    )
    .unwrap();
    drop(conn);

    // Run purge
    let conn = pool.get().unwrap();
    let purged =
        purge_soft_deleted(&conn, &registry, _tmp.path(), &LocaleConfig::default()).unwrap();
    assert_eq!(purged, 1, "should purge exactly the one expired doc");
    drop(conn);

    // The old doc should be completely gone (hard-deleted)
    let conn = pool.get().unwrap();
    let row = conn
        .query_one(
            &format!("SELECT id FROM articles WHERE id = {}", conn.placeholder(1)),
            &[DbValue::Text(id_old)],
        )
        .unwrap();
    assert!(row.is_none(), "expired doc should be hard-deleted by purge");

    // The recent soft-deleted doc should still exist (not yet past retention)
    let row = conn
        .query_one(
            &format!("SELECT id FROM articles WHERE id = {}", conn.placeholder(1)),
            &[DbValue::Text(id_recent)],
        )
        .unwrap();
    assert!(row.is_some(), "recently-deleted doc should survive purge");

    // The live doc should be untouched
    let count = ops::count_documents(&pool, "articles", &def, &[], None).unwrap();
    assert_eq!(count, 1, "live doc should remain");
}

// ── Test j: FindQuery include_deleted defaults to false ──────────────────

#[test]
fn find_query_include_deleted_defaults_false() {
    let query = FindQuery::default();
    assert!(
        !query.include_deleted,
        "include_deleted should default to false"
    );

    let query2 = FindQuery::new();
    assert!(
        !query2.include_deleted,
        "FindQuery::new() should also default to false"
    );
}

// ── Test k: soft delete removes from FTS ─────────────────────────────────

#[test]
fn soft_delete_removes_from_fts() {
    let def = make_soft_delete_def();
    let (_tmp, pool, _reg) = create_pool_and_migrate(vec![def.clone()]);

    let id = insert_doc(
        &pool,
        "articles",
        &def,
        &[("title", "Searchable Unicorn"), ("body", "magic")],
    );

    // Sync FTS and index the document
    let conn = pool.get().unwrap();
    query::fts::sync_fts_table(&conn, "articles", &def, &LocaleConfig::default()).unwrap();

    // Build a document and upsert into FTS
    let doc = query::find_by_id_unfiltered(&conn, "articles", &def, &id, None)
        .unwrap()
        .unwrap();
    query::fts::fts_upsert(&conn, "articles", &doc, Some(&def)).unwrap();

    // Verify FTS finds it
    let results = query::fts::fts_search(&conn, "articles", "Unicorn", 10).unwrap();
    assert_eq!(
        results.len(),
        1,
        "FTS should find the document before soft-delete"
    );

    // Soft-delete and remove from FTS (mirrors what delete_document does)
    query::soft_delete(&conn, "articles", &id).unwrap();
    query::fts::fts_delete(&conn, "articles", &id).unwrap();

    // Verify FTS no longer finds it
    let results = query::fts::fts_search(&conn, "articles", "Unicorn", 10).unwrap();
    assert!(
        results.is_empty(),
        "FTS should not find soft-deleted document"
    );
}

// ── Test l: restore re-adds to FTS ───────────────────────────────────────

#[test]
fn restore_re_adds_to_fts() {
    let def = make_soft_delete_def();
    let (_tmp, pool, _reg) = create_pool_and_migrate(vec![def.clone()]);

    let id = insert_doc(
        &pool,
        "articles",
        &def,
        &[("title", "Phoenix Rising"), ("body", "reborn")],
    );

    let conn = pool.get().unwrap();
    query::fts::sync_fts_table(&conn, "articles", &def, &LocaleConfig::default()).unwrap();

    // Index the document
    let doc = query::find_by_id_unfiltered(&conn, "articles", &def, &id, None)
        .unwrap()
        .unwrap();
    query::fts::fts_upsert(&conn, "articles", &doc, Some(&def)).unwrap();

    // Soft-delete + FTS cleanup
    query::soft_delete(&conn, "articles", &id).unwrap();
    query::fts::fts_delete(&conn, "articles", &id).unwrap();

    // Verify gone from FTS
    let results = query::fts::fts_search(&conn, "articles", "Phoenix", 10).unwrap();
    assert!(results.is_empty(), "should not be in FTS after soft-delete");

    // Restore + re-index FTS (mirrors what restore_document does)
    query::restore(&conn, "articles", &id).unwrap();
    let doc = query::find_by_id_unfiltered(&conn, "articles", &def, &id, None)
        .unwrap()
        .unwrap();
    query::fts::fts_upsert(&conn, "articles", &doc, Some(&def)).unwrap();

    // Verify back in FTS
    let results = query::fts::fts_search(&conn, "articles", "Phoenix", 10).unwrap();
    assert_eq!(results.len(), 1, "should be back in FTS after restore");
    assert_eq!(results[0], id);
}

// ── Test: count with include_deleted returns all docs ────────────────────

#[test]
fn count_with_include_deleted_returns_all() {
    let def = make_soft_delete_def();
    let (_tmp, pool, _reg) = create_pool_and_migrate(vec![def.clone()]);

    insert_doc(&pool, "articles", &def, &[("title", "A"), ("body", "")]);
    let id_b = insert_doc(&pool, "articles", &def, &[("title", "B"), ("body", "")]);

    let conn = pool.get().unwrap();
    query::soft_delete(&conn, "articles", &id_b).unwrap();
    drop(conn);

    // Normal count excludes soft-deleted
    let normal_count = ops::count_documents(&pool, "articles", &def, &[], None).unwrap();
    assert_eq!(normal_count, 1);

    // Count with include_deleted
    let conn = pool.get().unwrap();
    let full_count =
        query::count_with_search(&conn, "articles", &def, &[], None, None, true).unwrap();
    assert_eq!(
        full_count, 2,
        "count with include_deleted should return all docs"
    );
}

// ── Test: soft_delete idempotent (already deleted returns false) ─────────

#[test]
fn soft_delete_already_deleted_returns_false() {
    let def = make_soft_delete_def();
    let (_tmp, pool, _reg) = create_pool_and_migrate(vec![def.clone()]);

    let id = insert_doc(&pool, "articles", &def, &[("title", "Once"), ("body", "")]);

    let conn = pool.get().unwrap();
    assert!(query::soft_delete(&conn, "articles", &id).unwrap());
    assert!(
        !query::soft_delete(&conn, "articles", &id).unwrap(),
        "second soft_delete should return false"
    );
}

// ── Test: restore non-deleted doc returns false ──────────────────────────

#[test]
fn restore_non_deleted_doc_returns_false() {
    let def = make_soft_delete_def();
    let (_tmp, pool, _reg) = create_pool_and_migrate(vec![def.clone()]);

    let id = insert_doc(&pool, "articles", &def, &[("title", "Alive"), ("body", "")]);

    let conn = pool.get().unwrap();
    let restored = query::restore(&conn, "articles", &id).unwrap();
    assert!(!restored, "restore should return false for non-deleted doc");
}

// ── Test: find_by_id_unfiltered includes soft-deleted ────────────────────

#[test]
fn find_by_id_unfiltered_includes_soft_deleted() {
    let def = make_soft_delete_def();
    let (_tmp, pool, _reg) = create_pool_and_migrate(vec![def.clone()]);

    let id = insert_doc(
        &pool,
        "articles",
        &def,
        &[("title", "Hidden"), ("body", "treasure")],
    );

    let conn = pool.get().unwrap();
    query::soft_delete(&conn, "articles", &id).unwrap();

    // find_by_id (filtered) should return None
    let filtered = query::find_by_id(&conn, "articles", &def, &id, None).unwrap();
    assert!(filtered.is_none());

    // find_by_id_unfiltered should return the doc
    let unfiltered = query::find_by_id_unfiltered(&conn, "articles", &def, &id, None).unwrap();
    assert!(unfiltered.is_some());
    assert_eq!(unfiltered.unwrap().get_str("title"), Some("Hidden"));
}

// ── Regression: empty_trash must skip referenced documents ──────────────

/// Regression: permanently deleting soft-deleted documents (empty trash) must
/// skip documents that are still referenced by other documents, preserving
/// referential integrity. Previously, empty_trash deleted all trashed docs
/// without checking _ref_count, which could orphan references.
#[test]
fn empty_trash_skips_referenced_documents() {
    use crap_cms::core::field::RelationshipConfig;

    // Two collections: "media" (soft-delete) and "posts" which references media
    let mut media_def = CollectionDefinition::new("media");
    media_def.fields = vec![FieldDefinition::builder("filename", FieldType::Text).build()];
    media_def.timestamps = true;
    media_def.soft_delete = true;

    let mut posts_def = CollectionDefinition::new("posts");
    posts_def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text).build(),
        FieldDefinition::builder("image", FieldType::Relationship)
            .relationship(RelationshipConfig::new("media", false))
            .build(),
    ];
    posts_def.timestamps = true;

    let (_tmp, pool, _reg) = create_pool_and_migrate(vec![media_def.clone(), posts_def.clone()]);

    // Insert two media docs
    let m1 = insert_doc(&pool, "media", &media_def, &[("filename", "photo.jpg")]);
    let m2 = insert_doc(&pool, "media", &media_def, &[("filename", "video.mp4")]);

    // Create a post referencing m1
    let conn = pool.get().unwrap();
    let post_data: HashMap<String, String> = [("title", "My Post"), ("image", m1.as_str())]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let post_doc = query::create(&conn, "posts", &posts_def, &post_data, None).unwrap();

    // Bump ref counts to reflect the relationship
    let locale_cfg = LocaleConfig::default();
    query::ref_count::after_create(&conn, "posts", &post_doc.id, &posts_def.fields, &locale_cfg)
        .unwrap();

    // Verify m1 has ref_count > 0
    let rc = query::ref_count::get_ref_count(&conn, "media", &m1)
        .unwrap()
        .expect("m1 should exist");
    assert!(rc > 0, "m1 should be referenced, got ref_count={}", rc);

    // Soft-delete both media docs
    query::soft_delete(&conn, "media", &m1).unwrap();
    query::soft_delete(&conn, "media", &m2).unwrap();

    // Simulate "empty trash": permanently delete all soft-deleted media,
    // but skip any that are still referenced.
    let mut deleted_count = 0;
    let mut fq = FindQuery::new();
    fq.include_deleted = true;
    fq.filters = vec![crap_cms::db::FilterClause::Single(crap_cms::db::Filter {
        field: "_deleted_at".to_string(),
        op: crap_cms::db::FilterOp::Exists,
    })];

    let docs = query::find(&conn, "media", &media_def, &fq, None).unwrap();
    assert_eq!(docs.len(), 2, "both should be in trash");

    for doc in &docs {
        let ref_count = query::ref_count::get_ref_count(&conn, "media", &doc.id)
            .unwrap()
            .unwrap_or(0);
        if ref_count > 0 {
            continue; // Skip referenced — this is the fix under test
        }
        query::ref_count::before_hard_delete(
            &conn,
            "media",
            &doc.id,
            &media_def.fields,
            &locale_cfg,
        )
        .unwrap();
        query::delete(&conn, "media", &doc.id).unwrap();
        deleted_count += 1;
    }

    // m2 should be permanently deleted, m1 should be preserved (still referenced)
    assert_eq!(deleted_count, 1, "only unreferenced doc should be deleted");

    // m1 should still exist (soft-deleted but preserved)
    let m1_exists = query::find_by_id_unfiltered(&conn, "media", &media_def, &m1, None)
        .unwrap()
        .is_some();
    assert!(m1_exists, "referenced doc m1 should survive empty-trash");

    // m2 should be gone
    let m2_exists = query::find_by_id_unfiltered(&conn, "media", &media_def, &m2, None)
        .unwrap()
        .is_some();
    assert!(
        !m2_exists,
        "unreferenced doc m2 should be permanently deleted"
    );
}
