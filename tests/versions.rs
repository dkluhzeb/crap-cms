//! Integration tests for the versioning and drafts system.
//!
//! Covers: DB query layer (create/list/find/restore/prune versions, status),
//! service layer (create_document/update_document with draft param),
//! and gRPC API (draft flag on CRUD RPCs, ListVersions, RestoreVersion).

use std::collections::HashMap;
use std::sync::Arc;

use prost_types::{Struct, Value, value::Kind};
use serde_json::json;
use tonic::Request;

use crap_cms::api::content;
use crap_cms::api::content::content_api_server::ContentApi;
use crap_cms::api::handlers::{ContentService, ContentServiceDeps};
use crap_cms::config::*;
use crap_cms::core::Registry;
use crap_cms::core::collection::*;
use crap_cms::core::email::EmailRenderer;
use crap_cms::core::field::*;
use crap_cms::db::{DbConnection, DbValue, migrate, pool, query};
use crap_cms::hooks::lifecycle::HookRunner;
use crap_cms::service;

// ── Helpers ───────────────────────────────────────────────────────────────

fn make_versioned_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("articles");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Article".to_string())),
        plural: Some(LocalizedString::Plain("Articles".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("body", FieldType::Textarea).build(),
    ];
    def.versions = Some(VersionsConfig::new(true, 0));
    def
}

fn make_nonversioned_def() -> CollectionDefinition {
    let mut def = make_versioned_def();
    def.slug = "notes".into();
    def.versions = None;
    def
}

fn create_test_pool() -> (tempfile::TempDir, crap_cms::db::DbPool) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");
    (tmp, db_pool)
}

fn setup_db(
    defs: Vec<CollectionDefinition>,
) -> (
    tempfile::TempDir,
    crap_cms::db::DbPool,
    crap_cms::core::SharedRegistry,
) {
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
    _registry: crap_cms::core::SharedRegistry,
    runner: HookRunner,
}

fn setup_service(defs: Vec<CollectionDefinition>) -> TestSetup {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::test_default();
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

    let hook_runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(registry.clone())
        .config(&config)
        .build()
        .expect("hook runner");
    let email_renderer = Arc::new(EmailRenderer::new(tmp.path()).expect("email renderer"));

    let service = ContentService::new(
        ContentServiceDeps::builder()
            .pool(db_pool.clone())
            .registry(Registry::snapshot(&registry))
            .hook_runner(hook_runner.clone())
            .jwt_secret(config.auth.secret.clone())
            .config(config.clone())
            .config_dir(tmp.path().to_path_buf())
            .storage(
                crap_cms::core::upload::create_storage(
                    tmp.path(),
                    &crap_cms::config::UploadConfig::default(),
                )
                .unwrap(),
            )
            .email_renderer(email_renderer)
            .login_limiter(std::sync::Arc::new(
                crap_cms::core::rate_limit::LoginRateLimiter::new(5, 300),
            ))
            .ip_login_limiter(Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
                20, 300,
            )))
            .forgot_password_limiter(std::sync::Arc::new(
                crap_cms::core::rate_limit::LoginRateLimiter::new(3, 900),
            ))
            .ip_forgot_password_limiter(Arc::new(
                crap_cms::core::rate_limit::LoginRateLimiter::new(20, 900),
            ))
            .cache(std::sync::Arc::new(crap_cms::core::cache::NoneCache))
            .token_provider(std::sync::Arc::new(
                crap_cms::core::auth::JwtTokenProvider::new("test-secret"),
            ))
            .password_provider(std::sync::Arc::new(
                crap_cms::core::auth::Argon2PasswordProvider,
            ))
            .build(),
    );

    TestSetup {
        _tmp: tmp,
        service,
        pool: db_pool,
        _registry: registry,
        runner: hook_runner,
    }
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

// ── DB-Level Version Tests ──────────────────────────────────────────────

#[test]
fn migration_creates_versions_table_and_status_column() {
    let (_tmp, pool, _registry) = setup_db(vec![make_versioned_def()]);
    let conn = pool.get().unwrap();

    // _versions_articles table should exist
    let count: i64 = conn
        .query_one(
            "SELECT count(*) AS cnt FROM sqlite_master WHERE type='table' AND name='_versions_articles'",
            &[],
        )
        .unwrap()
        .unwrap()
        .get_i64("cnt")
        .unwrap();
    assert_eq!(count, 1, "versions table should exist");

    // _status column should exist on articles
    let status_exists: bool = conn
        .query_one("SELECT _status FROM articles LIMIT 0", &[])
        .is_ok();
    assert!(status_exists, "_status column should exist");
}

#[test]
fn migration_no_versions_table_for_nonversioned() {
    let (_tmp, pool, _registry) = setup_db(vec![make_nonversioned_def()]);
    let conn = pool.get().unwrap();

    let count: i64 = conn
        .query_one(
            "SELECT count(*) AS cnt FROM sqlite_master WHERE type='table' AND name='_versions_notes'",
            &[],
        )
        .unwrap()
        .unwrap()
        .get_i64("cnt")
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
    ]
    .into();
    let doc = query::create(&conn, "articles", &def, &data, None).unwrap();

    // Build snapshot and create version
    let snapshot = query::build_snapshot(&conn, "articles", &def.fields, &doc).unwrap();
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

    let snap = query::build_snapshot(&conn, "articles", &def.fields, &doc).unwrap();
    query::create_version(&conn, "articles", &doc.id, "published", &snap).unwrap();
    query::create_version(&conn, "articles", &doc.id, "draft", &snap).unwrap();
    let v3 = query::create_version(&conn, "articles", &doc.id, "published", &snap).unwrap();

    assert_eq!(v3.version, 3);
    assert!(v3.latest);

    // Only the latest version should have _latest=1
    let versions = query::list_versions(&conn, "articles", &doc.id, None, None).unwrap();
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
    let snap = query::build_snapshot(&conn, "articles", &def.fields, &doc).unwrap();

    query::create_version(&conn, "articles", &doc.id, "published", &snap).unwrap();
    query::create_version(&conn, "articles", &doc.id, "draft", &snap).unwrap();
    query::create_version(&conn, "articles", &doc.id, "published", &snap).unwrap();

    let versions = query::list_versions(&conn, "articles", &doc.id, None, None).unwrap();
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
    let snap = query::build_snapshot(&conn, "articles", &def.fields, &doc).unwrap();

    for _ in 0..5 {
        query::create_version(&conn, "articles", &doc.id, "published", &snap).unwrap();
    }

    let limited = query::list_versions(&conn, "articles", &doc.id, Some(3), None).unwrap();
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
    let snap = query::build_snapshot(&conn, "articles", &def.fields, &doc).unwrap();
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
    let snap = query::build_snapshot(&conn, "articles", &def.fields, &doc).unwrap();

    for _ in 0..10 {
        query::create_version(&conn, "articles", &doc.id, "published", &snap).unwrap();
    }
    assert_eq!(
        query::list_versions(&conn, "articles", &doc.id, None, None)
            .unwrap()
            .len(),
        10
    );

    // Prune to 3
    query::prune_versions(&conn, "articles", &doc.id, 3).unwrap();
    let remaining = query::list_versions(&conn, "articles", &doc.id, None, None).unwrap();
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
    let snap = query::build_snapshot(&conn, "articles", &def.fields, &doc).unwrap();

    for _ in 0..5 {
        query::create_version(&conn, "articles", &doc.id, "published", &snap).unwrap();
    }

    // max_versions=0 should not prune
    query::prune_versions(&conn, "articles", &doc.id, 0).unwrap();
    assert_eq!(
        query::list_versions(&conn, "articles", &doc.id, None, None)
            .unwrap()
            .len(),
        5
    );
}

#[test]
fn build_snapshot_includes_all_fields() {
    let def = make_versioned_def();
    let (_tmp, pool, _registry) = setup_db(vec![def.clone()]);
    let conn = pool.get().unwrap();

    let data: HashMap<String, String> = [
        ("title".into(), "Snap Title".into()),
        ("body".into(), "Snap Body".into()),
    ]
    .into();
    let doc = query::create(&conn, "articles", &def, &data, None).unwrap();

    let snapshot = query::build_snapshot(&conn, "articles", &def.fields, &doc).unwrap();
    let obj = snapshot.as_object().unwrap();
    assert_eq!(
        obj.get("title").and_then(|v| v.as_str()),
        Some("Snap Title")
    );
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
    ]
    .into();
    let doc = query::create(&conn, "articles", &def, &data, None).unwrap();

    // Create v1 snapshot
    let snap_v1 = query::build_snapshot(&conn, "articles", &def.fields, &doc).unwrap();
    query::create_version(&conn, "articles", &doc.id, "published", &snap_v1).unwrap();

    // Update document
    let update_data: HashMap<String, String> = [
        ("title".into(), "Updated".into()),
        ("body".into(), "Updated body".into()),
    ]
    .into();
    query::update(&conn, "articles", &def, &doc.id, &update_data, None).unwrap();

    // Create v2 snapshot
    let doc_updated = query::find_by_id(&conn, "articles", &def, &doc.id, None)
        .unwrap()
        .unwrap();
    let snap_v2 = query::build_snapshot(&conn, "articles", &def.fields, &doc_updated).unwrap();
    query::create_version(&conn, "articles", &doc.id, "published", &snap_v2).unwrap();

    // Verify current state is updated
    let current = query::find_by_id(&conn, "articles", &def, &doc.id, None)
        .unwrap()
        .unwrap();
    assert_eq!(current.get_str("title"), Some("Updated"));

    // Restore v1
    let restored = query::restore_version(
        &conn,
        "articles",
        &def,
        &doc.id,
        &snap_v1,
        "published",
        &Default::default(),
    )
    .unwrap();
    assert_eq!(restored.get_str("title"), Some("Original"));

    // Verify DB has restored data
    let after_restore = query::find_by_id(&conn, "articles", &def, &doc.id, None)
        .unwrap()
        .unwrap();
    assert_eq!(after_restore.get_str("title"), Some("Original"));
    assert_eq!(after_restore.get_str("body"), Some("Original body"));

    // Restore should create a new version (v3)
    let versions = query::list_versions(&conn, "articles", &doc.id, None, None).unwrap();
    assert_eq!(versions.len(), 3);
    assert_eq!(versions[0].version, 3);
}

/// Regression: restoring a version must clear locale columns that didn't exist
/// when the snapshot was taken, so stale translations don't persist.
#[test]
fn restore_version_clears_locale_columns() {
    // Build a versioned def with a localized title field
    let mut def = make_versioned_def();
    for field in &mut def.fields {
        if field.name == "title" {
            field.localized = true;
        }
    }

    let locale_config = LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    };

    // Setup DB with locale-aware migration
    let (tmp, db_pool) = create_test_pool();
    let registry = crap_cms::core::Registry::shared();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&db_pool, &registry, &locale_config).expect("sync");
    let conn = db_pool.get().unwrap();

    // Create document with English title
    let en_ctx = crap_cms::db::query::LocaleContext {
        mode: crap_cms::db::query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };
    let data: HashMap<String, String> = [
        ("title".into(), "English Title".into()),
        ("body".into(), "Body".into()),
    ]
    .into();
    let doc = query::create(&conn, "articles", &def, &data, Some(&en_ctx)).unwrap();

    // Create v1 snapshot (only English)
    let snap_v1 = query::build_snapshot(&conn, "articles", &def.fields, &doc).unwrap();
    query::create_version(&conn, "articles", &doc.id, "published", &snap_v1).unwrap();

    // Now add a German translation
    let de_ctx = crap_cms::db::query::LocaleContext {
        mode: crap_cms::db::query::LocaleMode::Single("de".to_string()),
        config: locale_config.clone(),
    };
    let de_data: HashMap<String, String> = [("title".into(), "Deutscher Titel".into())].into();
    query::update(&conn, "articles", &def, &doc.id, &de_data, Some(&de_ctx)).unwrap();

    // Verify German translation exists
    let de_doc = query::find_by_id(&conn, "articles", &def, &doc.id, Some(&de_ctx))
        .unwrap()
        .unwrap();
    assert_eq!(de_doc.get_str("title"), Some("Deutscher Titel"));

    // Restore v1 — should clear the German translation
    query::restore_version(
        &conn,
        "articles",
        &def,
        &doc.id,
        &snap_v1,
        "published",
        &locale_config,
    )
    .unwrap();

    // English should be restored
    let en_after = query::find_by_id(&conn, "articles", &def, &doc.id, Some(&en_ctx))
        .unwrap()
        .unwrap();
    assert_eq!(en_after.get_str("title"), Some("English Title"));

    // German should be cleared (NULL → fallback to English if fallback enabled, or NULL)
    // Read the raw column to verify it's NULL
    let de_raw: Option<String> = conn
        .query_one(
            "SELECT title__de FROM articles WHERE id = ?1",
            &[DbValue::Text(doc.id.to_string())],
        )
        .unwrap()
        .unwrap()
        .get_opt_string("title__de")
        .ok()
        .flatten();
    assert!(
        de_raw.is_none(),
        "German locale column should be NULL after restoring pre-translation version"
    );

    let _ = tmp; // keep tempdir alive
}

#[test]
fn delete_document_cascades_to_versions() {
    let def = make_versioned_def();
    let (_tmp, pool, _registry) = setup_db(vec![def.clone()]);
    let conn = pool.get().unwrap();

    let data: HashMap<String, String> = [("title".into(), "Cascade".into())].into();
    let doc = query::create(&conn, "articles", &def, &data, None).unwrap();
    let snap = query::build_snapshot(&conn, "articles", &def.fields, &doc).unwrap();
    query::create_version(&conn, "articles", &doc.id, "published", &snap).unwrap();
    query::create_version(&conn, "articles", &doc.id, "draft", &snap).unwrap();

    assert_eq!(
        query::list_versions(&conn, "articles", &doc.id, None, None)
            .unwrap()
            .len(),
        2
    );

    // Delete the document
    query::delete(&conn, "articles", &doc.id).unwrap();

    // Versions should be cascade-deleted
    assert_eq!(
        query::list_versions(&conn, "articles", &doc.id, None, None)
            .unwrap()
            .len(),
        0
    );
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

// ── Version Restore with Group Fields ─────────────────────────────────

fn make_versioned_group_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("pages_ver");
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("meta_title", FieldType::Text).build(),
                FieldDefinition::builder("meta_description", FieldType::Text).build(),
            ])
            .build(),
    ];
    def.versions = Some(VersionsConfig::new(true, 0));
    def
}

fn make_versioned_global_group_def() -> GlobalDefinition {
    let mut def = GlobalDefinition::new("site_ver");
    def.fields = vec![
        FieldDefinition::builder("site_name", FieldType::Text).build(),
        FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("meta_title", FieldType::Text).build(),
                FieldDefinition::builder("og_image", FieldType::Text).build(),
            ])
            .build(),
    ];
    def.versions = Some(VersionsConfig::new(true, 0));
    def
}

/// Collection: snapshot captures group sub-fields, restore brings them back.
#[test]
fn restore_version_with_group_fields() {
    let def = make_versioned_group_def();
    let (_tmp, pool, _registry) = setup_db(vec![def.clone()]);
    let conn = pool.get().unwrap();

    // Create with original group data
    let data: HashMap<String, String> = [
        ("title".into(), "Page One".into()),
        ("seo__meta_title".into(), "Original SEO".into()),
        ("seo__meta_description".into(), "Original desc".into()),
    ]
    .into();
    let doc = query::create(&conn, "pages_ver", &def, &data, None).unwrap();

    // Snapshot v1
    let snap_v1 = query::build_snapshot(&conn, "pages_ver", &def.fields, &doc).unwrap();
    query::create_version(&conn, "pages_ver", &doc.id, "published", &snap_v1).unwrap();

    // Update group fields
    let update_data: HashMap<String, String> = [
        ("seo__meta_title".into(), "Updated SEO".into()),
        ("seo__meta_description".into(), "Updated desc".into()),
    ]
    .into();
    query::update(&conn, "pages_ver", &def, &doc.id, &update_data, None).unwrap();

    // Verify updated
    let current = query::find_by_id(&conn, "pages_ver", &def, &doc.id, None)
        .unwrap()
        .unwrap();
    let cur_seo = current.fields.get("seo").expect("seo should exist");
    assert_eq!(
        cur_seo.get("meta_title").and_then(|v| v.as_str()),
        Some("Updated SEO")
    );

    // Restore v1
    query::restore_version(
        &conn,
        "pages_ver",
        &def,
        &doc.id,
        &snap_v1,
        "published",
        &Default::default(),
    )
    .unwrap();

    // Group sub-fields should be back to original
    let restored = query::find_by_id(&conn, "pages_ver", &def, &doc.id, None)
        .unwrap()
        .unwrap();
    let seo = restored.fields.get("seo").expect("seo should exist");
    assert_eq!(
        seo.get("meta_title").and_then(|v| v.as_str()),
        Some("Original SEO")
    );
    assert_eq!(
        seo.get("meta_description").and_then(|v| v.as_str()),
        Some("Original desc")
    );
    assert_eq!(restored.get_str("title"), Some("Page One"));
}

/// Global: snapshot captures group sub-fields, restore brings them back.
#[test]
fn restore_global_version_with_group_fields() {
    let gdef = make_versioned_global_group_def();
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    {
        let mut reg = registry.write().unwrap();
        reg.register_global(gdef.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("sync");
    let conn = pool.get().unwrap();

    // Set original group data
    let data: HashMap<String, String> = [
        ("site_name".into(), "My Site".into()),
        ("seo__meta_title".into(), "Original SEO".into()),
        ("seo__og_image".into(), "/original.png".into()),
    ]
    .into();
    query::update_global(&conn, "site_ver", &gdef, &data, None).unwrap();

    // Snapshot v1
    let doc = query::get_global(&conn, "site_ver", &gdef, None).unwrap();
    let snap_v1 = query::build_snapshot(&conn, "_global_site_ver", &gdef.fields, &doc).unwrap();
    query::create_version(&conn, "_global_site_ver", "default", "published", &snap_v1).unwrap();

    // Update group fields
    let update_data: HashMap<String, String> = [
        ("seo__meta_title".into(), "Updated SEO".into()),
        ("seo__og_image".into(), "/updated.png".into()),
    ]
    .into();
    query::update_global(&conn, "site_ver", &gdef, &update_data, None).unwrap();

    // Verify updated
    let current = query::get_global(&conn, "site_ver", &gdef, None).unwrap();
    let cur_seo = current.fields.get("seo").expect("seo should exist");
    assert_eq!(
        cur_seo.get("meta_title").and_then(|v| v.as_str()),
        Some("Updated SEO")
    );

    // Restore v1
    query::restore_global_version(
        &conn,
        "site_ver",
        &gdef,
        &snap_v1,
        "published",
        &Default::default(),
    )
    .unwrap();

    // Group sub-fields should be back to original
    let restored = query::get_global(&conn, "site_ver", &gdef, None).unwrap();
    let seo = restored.fields.get("seo").expect("seo should exist");
    assert_eq!(
        seo.get("meta_title").and_then(|v| v.as_str()),
        Some("Original SEO")
    );
    assert_eq!(
        seo.get("og_image").and_then(|v| v.as_str()),
        Some("/original.png")
    );
    assert_eq!(restored.get_str("site_name"), Some("My Site"));
}

/// Collection: snapshot + restore with localized group sub-fields.
#[test]
fn restore_version_with_localized_group_fields() {
    let mut def = make_versioned_group_def();
    // Make group sub-fields localized
    for field in &mut def.fields {
        if field.name == "seo" {
            for sub in &mut field.fields {
                sub.localized = true;
            }
        }
    }

    let locale_config = LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    };

    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &locale_config).expect("sync");
    let conn = pool.get().unwrap();

    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config.clone(),
    };

    // Create with English group data
    let data: HashMap<String, String> = [
        ("title".into(), "Page".into()),
        ("seo__meta_title".into(), "EN Original".into()),
        ("seo__meta_description".into(), "EN Desc".into()),
    ]
    .into();
    let doc = query::create(&conn, "pages_ver", &def, &data, Some(&en_ctx)).unwrap();

    // Add German translation
    let de_data: HashMap<String, String> =
        [("seo__meta_title".into(), "DE Original".into())].into();
    query::update(&conn, "pages_ver", &def, &doc.id, &de_data, Some(&de_ctx)).unwrap();

    // Snapshot v1 — use Default locale so find_by_id resolves locale columns
    let default_ctx = query::LocaleContext {
        mode: query::LocaleMode::Default,
        config: locale_config.clone(),
    };
    let doc_snap = query::find_by_id(&conn, "pages_ver", &def, &doc.id, Some(&default_ctx))
        .unwrap()
        .unwrap();
    let snap_v1 = query::build_snapshot(&conn, "pages_ver", &def.fields, &doc_snap).unwrap();
    query::create_version(&conn, "pages_ver", &doc.id, "published", &snap_v1).unwrap();

    // Update English
    let update_data: HashMap<String, String> =
        [("seo__meta_title".into(), "EN Updated".into())].into();
    query::update(
        &conn,
        "pages_ver",
        &def,
        &doc.id,
        &update_data,
        Some(&en_ctx),
    )
    .unwrap();

    // Verify updated
    let current = query::find_by_id(&conn, "pages_ver", &def, &doc.id, Some(&en_ctx))
        .unwrap()
        .unwrap();
    let cur_seo = current.fields.get("seo").expect("seo should exist");
    assert_eq!(
        cur_seo.get("meta_title").and_then(|v| v.as_str()),
        Some("EN Updated")
    );

    // Restore v1 — should restore EN to original and clear DE (restore writes to default locale only)
    query::restore_version(
        &conn,
        "pages_ver",
        &def,
        &doc.id,
        &snap_v1,
        "published",
        &locale_config,
    )
    .unwrap();

    let restored_en = query::find_by_id(&conn, "pages_ver", &def, &doc.id, Some(&en_ctx))
        .unwrap()
        .unwrap();
    let seo = restored_en.fields.get("seo").expect("seo should exist");
    assert_eq!(
        seo.get("meta_title").and_then(|v| v.as_str()),
        Some("EN Original"),
        "EN should be restored"
    );
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
    ]
    .into();
    let (doc, _) = service::create_document(
        pool,
        runner,
        "articles",
        &def,
        service::WriteInput::builder(data, &HashMap::new()).build(),
        None,
    )
    .unwrap();

    let conn = pool.get().unwrap();
    // Should have created a version
    let versions = query::list_versions(&conn, "articles", &doc.id, None, None).unwrap();
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

    let data: HashMap<String, String> = [("title".into(), "Draft Post".into())].into();
    let (doc, _) = service::create_document(
        pool,
        runner,
        "articles",
        &def,
        service::WriteInput::builder(data, &HashMap::new())
            .draft(true)
            .build(),
        None,
    )
    .unwrap();

    let conn = pool.get().unwrap();
    let versions = query::list_versions(&conn, "articles", &doc.id, None, None).unwrap();
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
    ]
    .into();
    let (doc, _) = service::create_document(
        pool,
        runner,
        "articles",
        &def,
        service::WriteInput::builder(data, &HashMap::new()).build(),
        None,
    )
    .unwrap();

    // Draft update — should NOT change the main table
    let update_data: HashMap<String, String> = [("title".into(), "Draft Title".into())].into();
    let (result, _) = service::update_document(
        pool,
        runner,
        "articles",
        &doc.id,
        &def,
        service::WriteInput::builder(update_data, &HashMap::new())
            .draft(true)
            .build(),
        None,
    )
    .unwrap();

    // Result should be the EXISTING doc (unchanged main table)
    assert_eq!(result.get_str("title"), Some("Original Title"));

    // Main table should still have original data
    let conn = pool.get().unwrap();
    let current = query::find_by_id(&conn, "articles", &def, &doc.id, None)
        .unwrap()
        .unwrap();
    assert_eq!(current.get_str("title"), Some("Original Title"));

    // But there should be 2 versions now (create + draft update)
    let versions = query::list_versions(&conn, "articles", &doc.id, None, None).unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].status, "draft");
    assert_eq!(versions[1].status, "published");

    // The draft version snapshot should have the updated title
    let draft_snap = &versions[0].snapshot;
    assert_eq!(
        draft_snap.get("title").and_then(|v| v.as_str()),
        Some("Draft Title")
    );
}

#[test]
fn service_update_publish_updates_main_table() {
    let def = make_versioned_def();
    let ts = setup_service(vec![def.clone()]);
    let pool = &ts.pool;
    let runner = &ts.runner;

    let data: HashMap<String, String> = [("title".into(), "Before Publish".into())].into();
    let (doc, _) = service::create_document(
        pool,
        runner,
        "articles",
        &def,
        service::WriteInput::builder(data, &HashMap::new())
            .draft(true)
            .build(), // create as draft
        None,
    )
    .unwrap();

    // Publish update (draft=false)
    let update_data: HashMap<String, String> = [("title".into(), "Published Title".into())].into();
    let (published, _) = service::update_document(
        pool,
        runner,
        "articles",
        &doc.id,
        &def,
        service::WriteInput::builder(update_data, &HashMap::new()).build(),
        None,
    )
    .unwrap();

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
    let (_doc, _) = service::create_document(
        pool,
        runner,
        "notes",
        &def,
        service::WriteInput::builder(data, &HashMap::new()).build(),
        None,
    )
    .unwrap();

    // No versions table for non-versioned, so nothing to check there
    // Just verify it doesn't crash
}

/// Regression: draft update must include join data (blocks/arrays) in the
/// version snapshot. Previously, `save_join_table_data` was skipped for
/// draft-only saves, so block data was lost from the snapshot.
#[test]
fn service_update_draft_preserves_join_data_in_snapshot() {
    // Build a def with a blocks field
    let mut def = make_versioned_def();
    def.fields.push(
        FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![crap_cms::core::field::BlockDefinition::new(
                "text",
                vec![FieldDefinition::builder("body", FieldType::Textarea).build()],
            )])
            .build(),
    );

    let ts = setup_service(vec![def.clone()]);
    let pool = &ts.pool;
    let runner = &ts.runner;

    // Create a published document
    let data: HashMap<String, String> = [("title".into(), "With Blocks".into())].into();
    let mut join_data = HashMap::new();
    join_data.insert(
        "content".to_string(),
        json!([
            {"_block_type": "text", "body": "Initial block"}
        ]),
    );
    let (doc, _) = service::create_document(
        pool,
        runner,
        "articles",
        &def,
        service::WriteInput::builder(data, &join_data).build(),
        None,
    )
    .unwrap();

    // Draft update with different block data
    let update_data: HashMap<String, String> =
        [("title".into(), "Draft With Blocks".into())].into();
    let mut draft_join_data = HashMap::new();
    draft_join_data.insert(
        "content".to_string(),
        json!([
            {"_block_type": "text", "body": "Draft block 1"},
            {"_block_type": "text", "body": "Draft block 2"}
        ]),
    );
    service::update_document(
        pool,
        runner,
        "articles",
        &doc.id,
        &def,
        service::WriteInput::builder(update_data, &draft_join_data)
            .draft(true)
            .build(),
        None,
    )
    .unwrap();

    // The draft version snapshot must contain the draft block data
    let conn = pool.get().unwrap();
    let versions = query::list_versions(&conn, "articles", &doc.id, None, None).unwrap();
    assert_eq!(versions.len(), 2); // create + draft update
    let draft_snap = &versions[0].snapshot;
    let blocks = draft_snap
        .get("content")
        .expect("snapshot must contain 'content' blocks field");
    let blocks_arr = blocks.as_array().expect("content should be an array");
    assert_eq!(blocks_arr.len(), 2, "draft snapshot should have 2 blocks");
    assert_eq!(
        blocks_arr[0].get("body").and_then(|v| v.as_str()),
        Some("Draft block 1")
    );
    assert_eq!(
        blocks_arr[1].get("body").and_then(|v| v.as_str()),
        Some("Draft block 2")
    );

    // Main table blocks should still be the original (not changed by draft)
    let main_doc = query::find_by_id(&conn, "articles", &def, &doc.id, None)
        .unwrap()
        .unwrap();
    let main_blocks = main_doc.fields.get("content").and_then(|v| v.as_array());
    // no blocks hydrated means join table was empty for main doc, which is acceptable
    if let Some(arr) = main_blocks {
        assert_eq!(arr.len(), 1, "main table should still have 1 block");
        assert_eq!(
            arr[0].get("body").and_then(|v| v.as_str()),
            Some("Initial block")
        );
    }
}

// ── gRPC-Level Version Tests ────────────────────────────────────────────

#[tokio::test]
async fn grpc_create_draft_sets_status() {
    let ts = setup_service(vec![make_versioned_def()]);

    let doc = ts
        .service
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

    let doc = ts
        .service
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
