//! Integration tests for the versioning and drafts system.
//!
//! Covers: DB query layer (create/list/find/restore/prune versions, status),
//! service layer (create_document/update_document with draft param),
//! and gRPC API (draft flag on CRUD RPCs, ListVersions, RestoreVersion).

use std::collections::HashMap;
use std::sync::Arc;

use prost_types::{value::Kind, Struct, Value};
use tonic::Request;

use crap_cms::api::content;
use crap_cms::api::content::content_api_server::ContentApi;
use crap_cms::api::service::ContentService;
use crap_cms::config::*;
use crap_cms::core::collection::*;
use crap_cms::core::email::EmailRenderer;
use crap_cms::core::field::*;
use crap_cms::core::Registry;
use crap_cms::db::{migrate, pool, query};
use crap_cms::service;
use crap_cms::hooks::lifecycle::HookRunner;

// ── Helpers ───────────────────────────────────────────────────────────────

fn make_versioned_def() -> CollectionDefinition {
    CollectionDefinition {
        slug: "articles".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("Article".to_string())),
            plural: Some(LocalizedString::Plain("Articles".to_string())),
        },
        timestamps: true,
        fields: vec![
            FieldDefinition {
                name: "title".to_string(),
                field_type: FieldType::Text,
                required: true,
                unique: false,
                validate: None,
                default_value: None,
                options: Vec::new(),
                admin: FieldAdmin::default(),
                hooks: FieldHooks::default(),
                access: FieldAccess::default(),
                relationship: None,
                fields: Vec::new(),
                blocks: Vec::new(),
                localized: false,
            },
            FieldDefinition {
                name: "body".to_string(),
                field_type: FieldType::Textarea,
                required: false,
                unique: false,
                validate: None,
                default_value: None,
                options: Vec::new(),
                admin: FieldAdmin::default(),
                hooks: FieldHooks::default(),
                access: FieldAccess::default(),
                relationship: None,
                fields: Vec::new(),
                blocks: Vec::new(),
                localized: false,
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        live: None,
        versions: Some(VersionsConfig {
            drafts: true,
            max_versions: 0,
        }),
    }
}

fn make_versioned_no_drafts_def() -> CollectionDefinition {
    let mut def = make_versioned_def();
    def.slug = "docs".to_string();
    def.versions = Some(VersionsConfig {
        drafts: false,
        max_versions: 5,
    });
    def
}

fn make_nonversioned_def() -> CollectionDefinition {
    let mut def = make_versioned_def();
    def.slug = "notes".to_string();
    def.versions = None;
    def
}

fn create_test_pool() -> (tempfile::TempDir, crap_cms::db::DbPool) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");
    (tmp, db_pool)
}

fn setup_db(defs: Vec<CollectionDefinition>) -> (tempfile::TempDir, crap_cms::db::DbPool, crap_cms::core::SharedRegistry) {
    let (tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    {
        let mut reg = registry.write().unwrap();
        for def in &defs {
            reg.register_collection(def.clone());
        }
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("sync");
    (tmp, pool, registry)
}

struct TestSetup {
    _tmp: tempfile::TempDir,
    service: ContentService,
    pool: crap_cms::db::DbPool,
    registry: crap_cms::core::SharedRegistry,
    runner: HookRunner,
}

fn setup_service(defs: Vec<CollectionDefinition>) -> TestSetup {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();

    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");
    let registry = Registry::shared();
    {
        let mut reg = registry.write().unwrap();
        for def in &defs {
            reg.register_collection(def.clone());
        }
    }
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync");

    let hook_runner = HookRunner::new(tmp.path(), registry.clone(), &config).expect("hook runner");
    let email_renderer = Arc::new(EmailRenderer::new(tmp.path()).expect("email renderer"));

    let service = ContentService::new(
        db_pool.clone(),
        registry.clone(),
        hook_runner.clone(),
        config.auth.secret.clone(),
        &config.depth,
        config.email.clone(),
        email_renderer,
        config.server.clone(),
        None,
        config.locale.clone(),
    );

    TestSetup { _tmp: tmp, service, pool: db_pool, registry, runner: hook_runner }
}

fn make_struct(fields: &[(&str, &str)]) -> Struct {
    let mut map = std::collections::BTreeMap::new();
    for (k, v) in fields {
        map.insert(
            k.to_string(),
            Value {
                kind: Some(Kind::StringValue(v.to_string())),
            },
        );
    }
    Struct { fields: map }
}

fn get_proto_field(doc: &content::Document, name: &str) -> Option<String> {
    doc.fields
        .as_ref()?
        .fields
        .get(name)
        .and_then(|v| match v.kind.as_ref()? {
            Kind::StringValue(s) => Some(s.clone()),
            Kind::NumberValue(n) => Some(n.to_string()),
            Kind::BoolValue(b) => Some(b.to_string()),
            _ => None,
        })
}

// ── DB-Level Version Tests ──────────────────────────────────────────────

#[test]
fn migration_creates_versions_table_and_status_column() {
    let (_tmp, pool, _registry) = setup_db(vec![make_versioned_def()]);
    let conn = pool.get().unwrap();

    // _versions_articles table should exist
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='_versions_articles'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "versions table should exist");

    // _status column should exist on articles
    let status_exists: bool = conn
        .prepare("SELECT _status FROM articles LIMIT 0")
        .is_ok();
    assert!(status_exists, "_status column should exist");
}

#[test]
fn migration_no_versions_table_for_nonversioned() {
    let (_tmp, pool, _registry) = setup_db(vec![make_nonversioned_def()]);
    let conn = pool.get().unwrap();

    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='_versions_notes'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "no versions table for non-versioned collection");
}

#[test]
fn create_version_and_find_latest() {
    let def = make_versioned_def();
    let (_tmp, pool, _registry) = setup_db(vec![def.clone()]);
    let conn = pool.get().unwrap();

    // Create a document first
    let data: HashMap<String, String> = [
        ("title".into(), "Version Test".into()),
        ("body".into(), "Initial content".into()),
    ].into();
    let doc = query::create(&conn, "articles", &def, &data, None).unwrap();

    // Build snapshot and create version
    let snapshot = query::build_snapshot(&conn, "articles", &def, &doc).unwrap();
    let v1 = query::create_version(&conn, "articles", &doc.id, "published", &snapshot).unwrap();

    assert_eq!(v1.version, 1);
    assert_eq!(v1.status, "published");
    assert!(v1.latest);
    assert_eq!(v1.parent, doc.id);

    // Find latest should return v1
    let latest = query::find_latest_version(&conn, "articles", &doc.id).unwrap();
    assert!(latest.is_some());
    let latest = latest.unwrap();
    assert_eq!(latest.version, 1);
    assert!(latest.latest);
}

#[test]
fn multiple_versions_latest_flag() {
    let def = make_versioned_def();
    let (_tmp, pool, _registry) = setup_db(vec![def.clone()]);
    let conn = pool.get().unwrap();

    let data: HashMap<String, String> = [("title".into(), "V1".into())].into();
    let doc = query::create(&conn, "articles", &def, &data, None).unwrap();

    let snap = query::build_snapshot(&conn, "articles", &def, &doc).unwrap();
    query::create_version(&conn, "articles", &doc.id, "published", &snap).unwrap();
    query::create_version(&conn, "articles", &doc.id, "draft", &snap).unwrap();
    let v3 = query::create_version(&conn, "articles", &doc.id, "published", &snap).unwrap();

    assert_eq!(v3.version, 3);
    assert!(v3.latest);

    // Only the latest version should have _latest=1
    let versions = query::list_versions(&conn, "articles", &doc.id, None).unwrap();
    assert_eq!(versions.len(), 3);
    // Newest first
    assert!(versions[0].latest);
    assert!(!versions[1].latest);
    assert!(!versions[2].latest);
}

#[test]
fn list_versions_newest_first() {
    let def = make_versioned_def();
    let (_tmp, pool, _registry) = setup_db(vec![def.clone()]);
    let conn = pool.get().unwrap();

    let data: HashMap<String, String> = [("title".into(), "Ordered".into())].into();
    let doc = query::create(&conn, "articles", &def, &data, None).unwrap();
    let snap = query::build_snapshot(&conn, "articles", &def, &doc).unwrap();

    query::create_version(&conn, "articles", &doc.id, "published", &snap).unwrap();
    query::create_version(&conn, "articles", &doc.id, "draft", &snap).unwrap();
    query::create_version(&conn, "articles", &doc.id, "published", &snap).unwrap();

    let versions = query::list_versions(&conn, "articles", &doc.id, None).unwrap();
    assert_eq!(versions.len(), 3);
    assert_eq!(versions[0].version, 3);
    assert_eq!(versions[1].version, 2);
    assert_eq!(versions[2].version, 1);
}

#[test]
fn list_versions_with_limit() {
    let def = make_versioned_def();
    let (_tmp, pool, _registry) = setup_db(vec![def.clone()]);
    let conn = pool.get().unwrap();

    let data: HashMap<String, String> = [("title".into(), "Limited".into())].into();
    let doc = query::create(&conn, "articles", &def, &data, None).unwrap();
    let snap = query::build_snapshot(&conn, "articles", &def, &doc).unwrap();

    for _ in 0..5 {
        query::create_version(&conn, "articles", &doc.id, "published", &snap).unwrap();
    }

    let limited = query::list_versions(&conn, "articles", &doc.id, Some(3)).unwrap();
    assert_eq!(limited.len(), 3);
    // Should be the 3 newest
    assert_eq!(limited[0].version, 5);
    assert_eq!(limited[2].version, 3);
}

#[test]
fn find_version_by_id_found_and_not_found() {
    let def = make_versioned_def();
    let (_tmp, pool, _registry) = setup_db(vec![def.clone()]);
    let conn = pool.get().unwrap();

    let data: HashMap<String, String> = [("title".into(), "FindById".into())].into();
    let doc = query::create(&conn, "articles", &def, &data, None).unwrap();
    let snap = query::build_snapshot(&conn, "articles", &def, &doc).unwrap();
    let v = query::create_version(&conn, "articles", &doc.id, "published", &snap).unwrap();

    // Find existing
    let found = query::find_version_by_id(&conn, "articles", &v.id).unwrap();
    assert!(found.is_some());
    assert_eq!(found.unwrap().version, 1);

    // Find non-existent
    let not_found = query::find_version_by_id(&conn, "articles", "nonexistent").unwrap();
    assert!(not_found.is_none());
}

#[test]
fn set_and_get_document_status() {
    let def = make_versioned_def();
    let (_tmp, pool, _registry) = setup_db(vec![def.clone()]);
    let conn = pool.get().unwrap();

    let data: HashMap<String, String> = [("title".into(), "Status Test".into())].into();
    let doc = query::create(&conn, "articles", &def, &data, None).unwrap();

    // Default status should be 'published' (the column default)
    let status = query::get_document_status(&conn, "articles", &doc.id).unwrap();
    assert_eq!(status.as_deref(), Some("published"));

    // Set to draft
    query::set_document_status(&conn, "articles", &doc.id, "draft").unwrap();
    let status = query::get_document_status(&conn, "articles", &doc.id).unwrap();
    assert_eq!(status.as_deref(), Some("draft"));

    // Set back to published
    query::set_document_status(&conn, "articles", &doc.id, "published").unwrap();
    let status = query::get_document_status(&conn, "articles", &doc.id).unwrap();
    assert_eq!(status.as_deref(), Some("published"));
}

#[test]
fn prune_versions_keeps_newest() {
    let def = make_versioned_def();
    let (_tmp, pool, _registry) = setup_db(vec![def.clone()]);
    let conn = pool.get().unwrap();

    let data: HashMap<String, String> = [("title".into(), "Prune Test".into())].into();
    let doc = query::create(&conn, "articles", &def, &data, None).unwrap();
    let snap = query::build_snapshot(&conn, "articles", &def, &doc).unwrap();

    for _ in 0..10 {
        query::create_version(&conn, "articles", &doc.id, "published", &snap).unwrap();
    }
    assert_eq!(query::list_versions(&conn, "articles", &doc.id, None).unwrap().len(), 10);

    // Prune to 3
    query::prune_versions(&conn, "articles", &doc.id, 3).unwrap();
    let remaining = query::list_versions(&conn, "articles", &doc.id, None).unwrap();
    assert_eq!(remaining.len(), 3);
    // Newest kept
    assert_eq!(remaining[0].version, 10);
    assert_eq!(remaining[1].version, 9);
    assert_eq!(remaining[2].version, 8);
}

#[test]
fn prune_versions_zero_means_unlimited() {
    let def = make_versioned_def();
    let (_tmp, pool, _registry) = setup_db(vec![def.clone()]);
    let conn = pool.get().unwrap();

    let data: HashMap<String, String> = [("title".into(), "No Prune".into())].into();
    let doc = query::create(&conn, "articles", &def, &data, None).unwrap();
    let snap = query::build_snapshot(&conn, "articles", &def, &doc).unwrap();

    for _ in 0..5 {
        query::create_version(&conn, "articles", &doc.id, "published", &snap).unwrap();
    }

    // max_versions=0 should not prune
    query::prune_versions(&conn, "articles", &doc.id, 0).unwrap();
    assert_eq!(query::list_versions(&conn, "articles", &doc.id, None).unwrap().len(), 5);
}

#[test]
fn build_snapshot_includes_all_fields() {
    let def = make_versioned_def();
    let (_tmp, pool, _registry) = setup_db(vec![def.clone()]);
    let conn = pool.get().unwrap();

    let data: HashMap<String, String> = [
        ("title".into(), "Snap Title".into()),
        ("body".into(), "Snap Body".into()),
    ].into();
    let doc = query::create(&conn, "articles", &def, &data, None).unwrap();

    let snapshot = query::build_snapshot(&conn, "articles", &def, &doc).unwrap();
    let obj = snapshot.as_object().unwrap();
    assert_eq!(obj.get("title").and_then(|v| v.as_str()), Some("Snap Title"));
    assert_eq!(obj.get("body").and_then(|v| v.as_str()), Some("Snap Body"));
    // Should include timestamps
    assert!(obj.contains_key("created_at"));
}

#[test]
fn restore_version_updates_main_table() {
    let def = make_versioned_def();
    let (_tmp, pool, _registry) = setup_db(vec![def.clone()]);
    let conn = pool.get().unwrap();

    // Create document with original data
    let data: HashMap<String, String> = [
        ("title".into(), "Original".into()),
        ("body".into(), "Original body".into()),
    ].into();
    let doc = query::create(&conn, "articles", &def, &data, None).unwrap();

    // Create v1 snapshot
    let snap_v1 = query::build_snapshot(&conn, "articles", &def, &doc).unwrap();
    query::create_version(&conn, "articles", &doc.id, "published", &snap_v1).unwrap();

    // Update document
    let update_data: HashMap<String, String> = [
        ("title".into(), "Updated".into()),
        ("body".into(), "Updated body".into()),
    ].into();
    query::update(&conn, "articles", &def, &doc.id, &update_data, None).unwrap();

    // Create v2 snapshot
    let doc_updated = query::find_by_id(&conn, "articles", &def, &doc.id, None).unwrap().unwrap();
    let snap_v2 = query::build_snapshot(&conn, "articles", &def, &doc_updated).unwrap();
    query::create_version(&conn, "articles", &doc.id, "published", &snap_v2).unwrap();

    // Verify current state is updated
    let current = query::find_by_id(&conn, "articles", &def, &doc.id, None).unwrap().unwrap();
    assert_eq!(current.get_str("title"), Some("Updated"));

    // Restore v1
    let restored = query::restore_version(&conn, "articles", &def, &doc.id, &snap_v1, "published").unwrap();
    assert_eq!(restored.get_str("title"), Some("Original"));

    // Verify DB has restored data
    let after_restore = query::find_by_id(&conn, "articles", &def, &doc.id, None).unwrap().unwrap();
    assert_eq!(after_restore.get_str("title"), Some("Original"));
    assert_eq!(after_restore.get_str("body"), Some("Original body"));

    // Restore should create a new version (v3)
    let versions = query::list_versions(&conn, "articles", &doc.id, None).unwrap();
    assert_eq!(versions.len(), 3);
    assert_eq!(versions[0].version, 3);
}

#[test]
fn delete_document_cascades_to_versions() {
    let def = make_versioned_def();
    let (_tmp, pool, _registry) = setup_db(vec![def.clone()]);
    let conn = pool.get().unwrap();

    let data: HashMap<String, String> = [("title".into(), "Cascade".into())].into();
    let doc = query::create(&conn, "articles", &def, &data, None).unwrap();
    let snap = query::build_snapshot(&conn, "articles", &def, &doc).unwrap();
    query::create_version(&conn, "articles", &doc.id, "published", &snap).unwrap();
    query::create_version(&conn, "articles", &doc.id, "draft", &snap).unwrap();

    assert_eq!(query::list_versions(&conn, "articles", &doc.id, None).unwrap().len(), 2);

    // Delete the document
    query::delete(&conn, "articles", &doc.id).unwrap();

    // Versions should be cascade-deleted
    assert_eq!(query::list_versions(&conn, "articles", &doc.id, None).unwrap().len(), 0);
}

#[test]
fn find_latest_version_returns_none_for_no_versions() {
    let def = make_versioned_def();
    let (_tmp, pool, _registry) = setup_db(vec![def.clone()]);
    let conn = pool.get().unwrap();

    let data: HashMap<String, String> = [("title".into(), "No Versions".into())].into();
    let doc = query::create(&conn, "articles", &def, &data, None).unwrap();

    let latest = query::find_latest_version(&conn, "articles", &doc.id).unwrap();
    assert!(latest.is_none());
}

// ── Service-Level Version Tests ────────────────────────────────────────

#[test]
fn service_create_published_creates_version() {
    let def = make_versioned_def();
    let ts = setup_service(vec![def.clone()]);
    let pool = &ts.pool;
    let runner = &ts.runner;

    let data: HashMap<String, String> = [
        ("title".into(), "Published".into()),
        ("body".into(), "Content".into()),
    ].into();
    let doc = service::create_document(
        pool, runner, "articles", &def, data, &HashMap::new(),
        None, None, None, None, false,
    ).unwrap();

    let conn = pool.get().unwrap();
    // Should have created a version
    let versions = query::list_versions(&conn, "articles", &doc.id, None).unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].status, "published");

    // Status should be published
    let status = query::get_document_status(&conn, "articles", &doc.id).unwrap();
    assert_eq!(status.as_deref(), Some("published"));
}

#[test]
fn service_create_draft_creates_draft_version() {
    let def = make_versioned_def();
    let ts = setup_service(vec![def.clone()]);
    let pool = &ts.pool;
    let runner = &ts.runner;

    let data: HashMap<String, String> = [
        ("title".into(), "Draft Post".into()),
    ].into();
    let doc = service::create_document(
        pool, runner, "articles", &def, data, &HashMap::new(),
        None, None, None, None, true,
    ).unwrap();

    let conn = pool.get().unwrap();
    let versions = query::list_versions(&conn, "articles", &doc.id, None).unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].status, "draft");

    let status = query::get_document_status(&conn, "articles", &doc.id).unwrap();
    assert_eq!(status.as_deref(), Some("draft"));
}

#[test]
fn service_update_draft_is_version_only() {
    let def = make_versioned_def();
    let ts = setup_service(vec![def.clone()]);
    let pool = &ts.pool;
    let runner = &ts.runner;

    // Create published document
    let data: HashMap<String, String> = [
        ("title".into(), "Original Title".into()),
        ("body".into(), "Original Body".into()),
    ].into();
    let doc = service::create_document(
        pool, runner, "articles", &def, data, &HashMap::new(),
        None, None, None, None, false,
    ).unwrap();

    // Draft update — should NOT change the main table
    let update_data: HashMap<String, String> = [
        ("title".into(), "Draft Title".into()),
    ].into();
    let result = service::update_document(
        pool, runner, "articles", &doc.id, &def, update_data, &HashMap::new(),
        None, None, None, None, true,
    ).unwrap();

    // Result should be the EXISTING doc (unchanged main table)
    assert_eq!(result.get_str("title"), Some("Original Title"));

    // Main table should still have original data
    let conn = pool.get().unwrap();
    let current = query::find_by_id(&conn, "articles", &def, &doc.id, None).unwrap().unwrap();
    assert_eq!(current.get_str("title"), Some("Original Title"));

    // But there should be 2 versions now (create + draft update)
    let versions = query::list_versions(&conn, "articles", &doc.id, None).unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].status, "draft");
    assert_eq!(versions[1].status, "published");

    // The draft version snapshot should have the updated title
    let draft_snap = &versions[0].snapshot;
    assert_eq!(draft_snap.get("title").and_then(|v| v.as_str()), Some("Draft Title"));
}

#[test]
fn service_update_publish_updates_main_table() {
    let def = make_versioned_def();
    let ts = setup_service(vec![def.clone()]);
    let pool = &ts.pool;
    let runner = &ts.runner;

    let data: HashMap<String, String> = [
        ("title".into(), "Before Publish".into()),
    ].into();
    let doc = service::create_document(
        pool, runner, "articles", &def, data, &HashMap::new(),
        None, None, None, None, true, // create as draft
    ).unwrap();

    // Publish update (draft=false)
    let update_data: HashMap<String, String> = [
        ("title".into(), "Published Title".into()),
    ].into();
    let published = service::update_document(
        pool, runner, "articles", &doc.id, &def, update_data, &HashMap::new(),
        None, None, None, None, false,
    ).unwrap();

    assert_eq!(published.get_str("title"), Some("Published Title"));

    let conn = pool.get().unwrap();
    let status = query::get_document_status(&conn, "articles", &doc.id).unwrap();
    assert_eq!(status.as_deref(), Some("published"));
}

#[test]
fn service_nonversioned_create_no_version_created() {
    let def = make_nonversioned_def();
    let ts = setup_service(vec![def.clone()]);
    let pool = &ts.pool;
    let runner = &ts.runner;

    let data: HashMap<String, String> = [("title".into(), "Note".into())].into();
    let _doc = service::create_document(
        pool, runner, "notes", &def, data, &HashMap::new(),
        None, None, None, None, false,
    ).unwrap();

    // No versions table for non-versioned, so nothing to check there
    // Just verify it doesn't crash
}

// ── gRPC-Level Version Tests ────────────────────────────────────────────

#[tokio::test]
async fn grpc_create_draft_sets_status() {
    let ts = setup_service(vec![make_versioned_def()]);

    let doc = ts.service
        .create(Request::new(content::CreateRequest {
            collection: "articles".to_string(),
            data: Some(make_struct(&[("title", "Draft Article")])),
            locale: None,
            draft: Some(true),
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // Verify via DB that status is draft
    let conn = ts.pool.get().unwrap();
    let status = query::get_document_status(&conn, "articles", &doc.id).unwrap();
    assert_eq!(status.as_deref(), Some("draft"));
}

#[tokio::test]
async fn grpc_create_published_sets_status() {
    let ts = setup_service(vec![make_versioned_def()]);

    let doc = ts.service
        .create(Request::new(content::CreateRequest {
            collection: "articles".to_string(),
            data: Some(make_struct(&[("title", "Published Article")])),
            locale: None,
            draft: Some(false),
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    let conn = ts.pool.get().unwrap();
    let status = query::get_document_status(&conn, "articles", &doc.id).unwrap();
    assert_eq!(status.as_deref(), Some("published"));
}

#[tokio::test]
async fn grpc_find_filters_by_published_status() {
    let ts = setup_service(vec![make_versioned_def()]);

    // Create one published and one draft
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "articles".to_string(),
            data: Some(make_struct(&[("title", "Published")])),
            locale: None,
            draft: Some(false),
        }))
        .await
        .unwrap();

    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "articles".to_string(),
            data: Some(make_struct(&[("title", "Draft")])),
            locale: None,
            draft: Some(true),
        }))
        .await
        .unwrap();

    // Find without draft flag — should only return published
    let resp = ts.service
        .find(Request::new(content::FindRequest {
            collection: "articles".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.total, 1);
    assert_eq!(get_proto_field(&resp.documents[0], "title").as_deref(), Some("Published"));
}

#[tokio::test]
async fn grpc_find_with_draft_returns_all() {
    let ts = setup_service(vec![make_versioned_def()]);

    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "articles".to_string(),
            data: Some(make_struct(&[("title", "Published")])),
            locale: None,
            draft: Some(false),
        }))
        .await
        .unwrap();

    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "articles".to_string(),
            data: Some(make_struct(&[("title", "Draft")])),
            locale: None,
            draft: Some(true),
        }))
        .await
        .unwrap();

    // Find with draft=true — should return all
    let resp = ts.service
        .find(Request::new(content::FindRequest {
            collection: "articles".to_string(),
            draft: Some(true),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.total, 2);
}

#[tokio::test]
async fn grpc_draft_update_is_version_only() {
    let ts = setup_service(vec![make_versioned_def()]);

    // Create published article
    let doc = ts.service
        .create(Request::new(content::CreateRequest {
            collection: "articles".to_string(),
            data: Some(make_struct(&[("title", "Original")])),
            locale: None,
            draft: Some(false),
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // Draft update
    ts.service
        .update(Request::new(content::UpdateRequest {
            collection: "articles".to_string(),
            id: doc.id.clone(),
            data: Some(make_struct(&[("title", "Draft Change")])),
            locale: None,
            draft: Some(true),
        }))
        .await
        .unwrap();

    // Regular find should still show "Original" (main table unchanged)
    let resp = ts.service
        .find(Request::new(content::FindRequest {
            collection: "articles".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.documents.len(), 1);
    assert_eq!(get_proto_field(&resp.documents[0], "title").as_deref(), Some("Original"));
}

#[tokio::test]
async fn grpc_find_by_id_draft_returns_latest_version() {
    let ts = setup_service(vec![make_versioned_def()]);

    // Create published article
    let doc = ts.service
        .create(Request::new(content::CreateRequest {
            collection: "articles".to_string(),
            data: Some(make_struct(&[("title", "Original")])),
            locale: None,
            draft: Some(false),
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // Draft update (version only)
    ts.service
        .update(Request::new(content::UpdateRequest {
            collection: "articles".to_string(),
            id: doc.id.clone(),
            data: Some(make_struct(&[("title", "Draft Title")])),
            locale: None,
            draft: Some(true),
        }))
        .await
        .unwrap();

    // FindByID with draft=true should load the draft version
    let resp = ts.service
        .find_by_id(Request::new(content::FindByIdRequest {
            collection: "articles".to_string(),
            id: doc.id.clone(),
            depth: Some(0),
            locale: None,
            select: vec![],
            draft: Some(true),
        }))
        .await
        .unwrap()
        .into_inner();

    let found = resp.document.unwrap();
    assert_eq!(get_proto_field(&found, "title").as_deref(), Some("Draft Title"));
}

#[tokio::test]
async fn grpc_find_by_id_no_draft_returns_main_table() {
    let ts = setup_service(vec![make_versioned_def()]);

    let doc = ts.service
        .create(Request::new(content::CreateRequest {
            collection: "articles".to_string(),
            data: Some(make_struct(&[("title", "Main Table")])),
            locale: None,
            draft: Some(false),
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // Draft update
    ts.service
        .update(Request::new(content::UpdateRequest {
            collection: "articles".to_string(),
            id: doc.id.clone(),
            data: Some(make_struct(&[("title", "Draft Only")])),
            locale: None,
            draft: Some(true),
        }))
        .await
        .unwrap();

    // FindByID without draft should return main table data
    let resp = ts.service
        .find_by_id(Request::new(content::FindByIdRequest {
            collection: "articles".to_string(),
            id: doc.id.clone(),
            depth: Some(0),
            locale: None,
            select: vec![],
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner();

    let found = resp.document.unwrap();
    assert_eq!(get_proto_field(&found, "title").as_deref(), Some("Main Table"));
}

#[tokio::test]
async fn grpc_list_versions() {
    let ts = setup_service(vec![make_versioned_def()]);

    let doc = ts.service
        .create(Request::new(content::CreateRequest {
            collection: "articles".to_string(),
            data: Some(make_struct(&[("title", "Versioned")])),
            locale: None,
            draft: Some(false),
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // Update a few times to create versions
    for title in &["V2", "V3"] {
        ts.service
            .update(Request::new(content::UpdateRequest {
                collection: "articles".to_string(),
                id: doc.id.clone(),
                data: Some(make_struct(&[("title", title)])),
                locale: None,
                draft: None,
            }))
            .await
            .unwrap();
    }

    let resp = ts.service
        .list_versions(Request::new(content::ListVersionsRequest {
            collection: "articles".to_string(),
            id: doc.id.clone(),
            limit: None,
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.versions.len(), 3);
    // Newest first
    assert_eq!(resp.versions[0].version, 3);
    assert_eq!(resp.versions[1].version, 2);
    assert_eq!(resp.versions[2].version, 1);
    // Only latest should be marked
    assert!(resp.versions[0].latest);
    assert!(!resp.versions[1].latest);
}

#[tokio::test]
async fn grpc_list_versions_with_limit() {
    let ts = setup_service(vec![make_versioned_def()]);

    let doc = ts.service
        .create(Request::new(content::CreateRequest {
            collection: "articles".to_string(),
            data: Some(make_struct(&[("title", "Many Versions")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    for i in 0..5 {
        ts.service
            .update(Request::new(content::UpdateRequest {
                collection: "articles".to_string(),
                id: doc.id.clone(),
                data: Some(make_struct(&[("title", &format!("Update {}", i))])),
                locale: None,
                draft: None,
            }))
            .await
            .unwrap();
    }

    let resp = ts.service
        .list_versions(Request::new(content::ListVersionsRequest {
            collection: "articles".to_string(),
            id: doc.id.clone(),
            limit: Some(3),
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.versions.len(), 3);
}

#[tokio::test]
async fn grpc_list_versions_nonversioned_fails() {
    let ts = setup_service(vec![make_nonversioned_def()]);

    let doc = ts.service
        .create(Request::new(content::CreateRequest {
            collection: "notes".to_string(),
            data: Some(make_struct(&[("title", "Note")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    let err = ts.service
        .list_versions(Request::new(content::ListVersionsRequest {
            collection: "notes".to_string(),
            id: doc.id.clone(),
            limit: None,
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
}

#[tokio::test]
async fn grpc_restore_version() {
    let ts = setup_service(vec![make_versioned_def()]);

    // Create article
    let doc = ts.service
        .create(Request::new(content::CreateRequest {
            collection: "articles".to_string(),
            data: Some(make_struct(&[("title", "Version 1")])),
            locale: None,
            draft: Some(false),
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // Get version 1 ID
    let v1_list = ts.service
        .list_versions(Request::new(content::ListVersionsRequest {
            collection: "articles".to_string(),
            id: doc.id.clone(),
            limit: None,
        }))
        .await
        .unwrap()
        .into_inner();
    let v1_id = v1_list.versions[0].id.clone();

    // Update to create v2
    ts.service
        .update(Request::new(content::UpdateRequest {
            collection: "articles".to_string(),
            id: doc.id.clone(),
            data: Some(make_struct(&[("title", "Version 2")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    // Verify current title is Version 2
    let current = ts.service
        .find_by_id(Request::new(content::FindByIdRequest {
            collection: "articles".to_string(),
            id: doc.id.clone(),
            depth: Some(0),
            locale: None,
            select: vec![],
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();
    assert_eq!(get_proto_field(&current, "title").as_deref(), Some("Version 2"));

    // Restore version 1
    let restored = ts.service
        .restore_version(Request::new(content::RestoreVersionRequest {
            collection: "articles".to_string(),
            document_id: doc.id.clone(),
            version_id: v1_id,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    assert_eq!(get_proto_field(&restored, "title").as_deref(), Some("Version 1"));

    // Should now have 3 versions (original + update + restore)
    let versions = ts.service
        .list_versions(Request::new(content::ListVersionsRequest {
            collection: "articles".to_string(),
            id: doc.id.clone(),
            limit: None,
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(versions.versions.len(), 3);
}

#[tokio::test]
async fn grpc_restore_version_nonversioned_fails() {
    let ts = setup_service(vec![make_nonversioned_def()]);

    let err = ts.service
        .restore_version(Request::new(content::RestoreVersionRequest {
            collection: "notes".to_string(),
            document_id: "fake".to_string(),
            version_id: "fake".to_string(),
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
}

#[tokio::test]
async fn grpc_describe_collection_shows_drafts() {
    let ts = setup_service(vec![make_versioned_def(), make_nonversioned_def()]);

    // Versioned with drafts
    let resp = ts.service
        .describe_collection(Request::new(content::DescribeCollectionRequest {
            slug: "articles".to_string(),
            is_global: false,
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(resp.drafts, "articles should have drafts=true");

    // Non-versioned
    let resp = ts.service
        .describe_collection(Request::new(content::DescribeCollectionRequest {
            slug: "notes".to_string(),
            is_global: false,
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(!resp.drafts, "notes should have drafts=false");
}

#[tokio::test]
async fn grpc_draft_create_skips_required_validation() {
    let ts = setup_service(vec![make_versioned_def()]);

    // Title is required, but draft=true should skip validation
    // Only providing body, no title
    let result = ts.service
        .create(Request::new(content::CreateRequest {
            collection: "articles".to_string(),
            data: Some(make_struct(&[("body", "Just a body, no title")])),
            locale: None,
            draft: Some(true),
        }))
        .await;

    // Should succeed because draft skips required
    assert!(result.is_ok(), "Draft create should skip required validation");
}

#[tokio::test]
async fn grpc_publish_enforces_required_validation() {
    let ts = setup_service(vec![make_versioned_def()]);

    // Create without required title and draft=false (publish)
    let result = ts.service
        .create(Request::new(content::CreateRequest {
            collection: "articles".to_string(),
            data: Some(make_struct(&[("body", "No title")])),
            locale: None,
            draft: Some(false),
        }))
        .await;

    // Should fail because title is required for publish
    assert!(result.is_err(), "Publish should enforce required validation");
}

#[tokio::test]
async fn grpc_versioned_no_drafts_does_not_filter_by_status() {
    // versions = { drafts = false } — has versioning but no draft/publish workflow
    let ts = setup_service(vec![make_versioned_no_drafts_def()]);

    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "docs".to_string(),
            data: Some(make_struct(&[("title", "Doc 1")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "docs".to_string(),
            data: Some(make_struct(&[("title", "Doc 2")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    // Find should return both (no _status filtering for drafts=false)
    let resp = ts.service
        .find(Request::new(content::FindRequest {
            collection: "docs".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.total, 2);
}

#[tokio::test]
async fn grpc_versioned_no_drafts_still_creates_versions() {
    let ts = setup_service(vec![make_versioned_no_drafts_def()]);

    let doc = ts.service
        .create(Request::new(content::CreateRequest {
            collection: "docs".to_string(),
            data: Some(make_struct(&[("title", "Versioned Doc")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    let resp = ts.service
        .list_versions(Request::new(content::ListVersionsRequest {
            collection: "docs".to_string(),
            id: doc.id.clone(),
            limit: None,
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.versions.len(), 1);
}

#[tokio::test]
async fn grpc_max_versions_prunes_old() {
    // docs collection has max_versions = 5
    let ts = setup_service(vec![make_versioned_no_drafts_def()]);

    let doc = ts.service
        .create(Request::new(content::CreateRequest {
            collection: "docs".to_string(),
            data: Some(make_struct(&[("title", "Prunable")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // Create 7 more updates (8 total versions: 1 create + 7 updates)
    for i in 0..7 {
        ts.service
            .update(Request::new(content::UpdateRequest {
                collection: "docs".to_string(),
                id: doc.id.clone(),
                data: Some(make_struct(&[("title", &format!("Update {}", i))])),
                locale: None,
                draft: None,
            }))
            .await
            .unwrap();
    }

    // max_versions=5, so only 5 should remain
    let resp = ts.service
        .list_versions(Request::new(content::ListVersionsRequest {
            collection: "docs".to_string(),
            id: doc.id.clone(),
            limit: None,
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.versions.len(), 5, "should be pruned to max_versions=5");
    // Newest should be version 8
    assert_eq!(resp.versions[0].version, 8);
}

#[tokio::test]
async fn grpc_full_draft_publish_workflow() {
    let ts = setup_service(vec![make_versioned_def()]);

    // 1. Create as draft
    let doc = ts.service
        .create(Request::new(content::CreateRequest {
            collection: "articles".to_string(),
            data: Some(make_struct(&[("title", "New Article"), ("body", "WIP content")])),
            locale: None,
            draft: Some(true),
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // 2. Should not be in regular find
    let resp = ts.service
        .find(Request::new(content::FindRequest {
            collection: "articles".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.total, 0, "draft should not appear in regular find");

    // 3. Should appear with draft=true
    let resp = ts.service
        .find(Request::new(content::FindRequest {
            collection: "articles".to_string(),
            draft: Some(true),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.total, 1, "draft should appear with draft=true");

    // 4. Make a draft update
    ts.service
        .update(Request::new(content::UpdateRequest {
            collection: "articles".to_string(),
            id: doc.id.clone(),
            data: Some(make_struct(&[("title", "Revised Article"), ("body", "Better content")])),
            locale: None,
            draft: Some(true),
        }))
        .await
        .unwrap();

    // 5. Publish the article
    ts.service
        .update(Request::new(content::UpdateRequest {
            collection: "articles".to_string(),
            id: doc.id.clone(),
            data: Some(make_struct(&[("title", "Final Article"), ("body", "Final content")])),
            locale: None,
            draft: Some(false),
        }))
        .await
        .unwrap();

    // 6. Should now appear in regular find
    let resp = ts.service
        .find(Request::new(content::FindRequest {
            collection: "articles".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.total, 1);
    assert_eq!(get_proto_field(&resp.documents[0], "title").as_deref(), Some("Final Article"));

    // 7. Should have 3 versions (create draft + draft update + publish)
    let versions = ts.service
        .list_versions(Request::new(content::ListVersionsRequest {
            collection: "articles".to_string(),
            id: doc.id.clone(),
            limit: None,
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(versions.versions.len(), 3);
    assert_eq!(versions.versions[0].status, "published");
    assert_eq!(versions.versions[1].status, "draft");
    assert_eq!(versions.versions[2].status, "draft");
}
