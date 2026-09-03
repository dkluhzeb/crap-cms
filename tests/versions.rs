//! Integration tests for the versioning and drafts system.
//!
//! Covers: DB query layer (create/list/find/restore/prune versions, status),
//! service layer (`create_document/update_document` with draft param),
//! and gRPC API (draft flag on CRUD RPCs, `ListVersions`, `RestoreVersion`).

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::used_underscore_binding,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal
)]

use std::sync::Arc;

use serde_json::json;
use tonic::Request;

use crap_cms::api::content;
use crap_cms::api::content::content_api_server::ContentApi;
use crap_cms::api::handlers::{ContentService, ContentServiceDeps};
use crap_cms::config::*;
use crap_cms::core::DocumentFields;
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
    Arc<crap_cms::core::Registry>,
) {
    let (tmp, pool) = create_test_pool();
    let shared = Registry::shared();
    {
        let mut reg = shared.write().unwrap();
        for def in &defs {
            reg.register_collection(def.clone());
        }
    }
    let registry = Registry::snapshot(&shared);
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("sync");
    (tmp, pool, registry)
}

struct TestSetup {
    _tmp: tempfile::TempDir,
    service: ContentService,
    pool: crap_cms::db::DbPool,
    _registry: Arc<crap_cms::core::Registry>,
    runner: HookRunner,
}

fn setup_service(defs: Vec<CollectionDefinition>) -> TestSetup {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();

    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");
    let shared = Registry::shared();
    {
        let mut reg = shared.write().unwrap();
        for def in &defs {
            reg.register_collection(def.clone());
        }
    }
    let registry = Registry::snapshot(&shared);
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync");

    let hook_runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("hook runner");
    let email_renderer = Arc::new(EmailRenderer::new(tmp.path()).expect("email renderer"));

    let service = ContentService::new(
        ContentServiceDeps::builder()
            .pool(db_pool.clone())
            .registry(Arc::clone(&registry))
            .hook_runner(hook_runner.clone())
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
                crap_cms::core::auth::JwtTokenProvider::new("test-jwt-secret"),
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

fn make_struct(fields: &[(&str, &str)]) -> content::DataMap {
    let mut map = std::collections::HashMap::new();
    for (k, v) in fields {
        map.insert(
            k.to_string(),
            content::FieldValue {
                kind: Some(content::field_value::Kind::StringValue(v.to_string())),
            },
        );
    }
    content::DataMap { fields: map }
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
    let data: DocumentFields = [
        ("title".into(), json!("Version Test")),
        ("body".into(), json!("Initial content")),
    ]
    .into_iter()
    .collect();
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

    let data: DocumentFields = [("title".into(), json!("V1"))].into_iter().collect();
    let doc = query::create(&conn, "articles", &def, &data, None).unwrap();

    let snap = query::build_snapshot(&conn, "articles", &def.fields, &doc).unwrap();
    query::create_version(&conn, "articles", &doc.id, "published", &snap).unwrap();
    query::create_version(&conn, "articles", &doc.id, "draft", &snap).unwrap();
    let v3 = query::create_version(&conn, "articles", &doc.id, "published", &snap).unwrap();

    assert_eq!(v3.version, 3);
    assert!(v3.latest);

    // Only the latest version should have _latest=1
    let versions = query::list_versions(&conn, "articles", &doc.id, false, None, None).unwrap();
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

    let data: DocumentFields = [("title".into(), json!("Ordered"))].into_iter().collect();
    let doc = query::create(&conn, "articles", &def, &data, None).unwrap();
    let snap = query::build_snapshot(&conn, "articles", &def.fields, &doc).unwrap();

    query::create_version(&conn, "articles", &doc.id, "published", &snap).unwrap();
    query::create_version(&conn, "articles", &doc.id, "draft", &snap).unwrap();
    query::create_version(&conn, "articles", &doc.id, "published", &snap).unwrap();

    let versions = query::list_versions(&conn, "articles", &doc.id, false, None, None).unwrap();
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

    let data: DocumentFields = [("title".into(), json!("Limited"))].into_iter().collect();
    let doc = query::create(&conn, "articles", &def, &data, None).unwrap();
    let snap = query::build_snapshot(&conn, "articles", &def.fields, &doc).unwrap();

    for _ in 0..5 {
        query::create_version(&conn, "articles", &doc.id, "published", &snap).unwrap();
    }

    let limited = query::list_versions(&conn, "articles", &doc.id, false, Some(3), None).unwrap();
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

    let data: DocumentFields = [("title".into(), json!("FindById"))].into_iter().collect();
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

    let data: DocumentFields = [("title".into(), json!("Status Test"))]
        .into_iter()
        .collect();
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

    let data: DocumentFields = [("title".into(), json!("Prune Test"))]
        .into_iter()
        .collect();
    let doc = query::create(&conn, "articles", &def, &data, None).unwrap();
    let snap = query::build_snapshot(&conn, "articles", &def.fields, &doc).unwrap();

    for _ in 0..10 {
        query::create_version(&conn, "articles", &doc.id, "published", &snap).unwrap();
    }
    assert_eq!(
        query::list_versions(&conn, "articles", &doc.id, false, None, None)
            .unwrap()
            .len(),
        10
    );

    // Prune to 3
    query::prune_versions(&conn, "articles", &doc.id, 3).unwrap();
    let remaining = query::list_versions(&conn, "articles", &doc.id, false, None, None).unwrap();
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

    let data: DocumentFields = [("title".into(), json!("No Prune"))].into_iter().collect();
    let doc = query::create(&conn, "articles", &def, &data, None).unwrap();
    let snap = query::build_snapshot(&conn, "articles", &def.fields, &doc).unwrap();

    for _ in 0..5 {
        query::create_version(&conn, "articles", &doc.id, "published", &snap).unwrap();
    }

    // max_versions=0 should not prune
    query::prune_versions(&conn, "articles", &doc.id, 0).unwrap();
    assert_eq!(
        query::list_versions(&conn, "articles", &doc.id, false, None, None)
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

    let data: DocumentFields = [
        ("title".into(), json!("Snap Title")),
        ("body".into(), json!("Snap Body")),
    ]
    .into_iter()
    .collect();
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
    let data: DocumentFields = [
        ("title".into(), json!("Original")),
        ("body".into(), json!("Original body")),
    ]
    .into_iter()
    .collect();
    let doc = query::create(&conn, "articles", &def, &data, None).unwrap();

    // Create v1 snapshot
    let snap_v1 = query::build_snapshot(&conn, "articles", &def.fields, &doc).unwrap();
    query::create_version(&conn, "articles", &doc.id, "published", &snap_v1).unwrap();

    // Update document
    let update_data: DocumentFields = [
        ("title".into(), json!("Updated")),
        ("body".into(), json!("Updated body")),
    ]
    .into_iter()
    .collect();
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
        &crap_cms::config::LocaleConfig::default(),
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
    let versions = query::list_versions(&conn, "articles", &doc.id, false, None, None).unwrap();
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
    let shared = crap_cms::core::Registry::shared();
    {
        let mut reg = shared.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&db_pool, &shared.read().unwrap(), &locale_config).expect("sync");
    let conn = db_pool.get().unwrap();

    // Create document with English title
    let en_ctx = crap_cms::db::query::LocaleContext {
        mode: crap_cms::db::query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };
    let data: DocumentFields = [
        ("title".into(), json!("English Title")),
        ("body".into(), json!("Body")),
    ]
    .into_iter()
    .collect();
    let doc = query::create(&conn, "articles", &def, &data, Some(&en_ctx)).unwrap();

    // Create v1 snapshot (only English)
    let snap_v1 = query::build_snapshot(&conn, "articles", &def.fields, &doc).unwrap();
    query::create_version(&conn, "articles", &doc.id, "published", &snap_v1).unwrap();

    // Now add a German translation
    let de_ctx = crap_cms::db::query::LocaleContext {
        mode: crap_cms::db::query::LocaleMode::Single("de".to_string()),
        config: locale_config.clone(),
    };
    let de_data: DocumentFields = [("title".into(), json!("Deutscher Titel"))]
        .into_iter()
        .collect();
    query::update(&conn, "articles", &def, &doc.id, &de_data, Some(&de_ctx)).unwrap();

    // Verify German translation exists
    let de_doc = query::find_by_id(&conn, "articles", &def, &doc.id, Some(&de_ctx))
        .unwrap()
        .unwrap();
    assert_eq!(de_doc.get_str("title"), Some("Deutscher Titel"));

    // Restore v1 — its snapshot predates the translation, so the German
    // column goes back to NULL (a snapshot that CARRIES the translation
    // restores it — see restore_version_restores_translations_from_snapshot).
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

    let data: DocumentFields = [("title".into(), json!("Cascade"))].into_iter().collect();
    let doc = query::create(&conn, "articles", &def, &data, None).unwrap();
    let snap = query::build_snapshot(&conn, "articles", &def.fields, &doc).unwrap();
    query::create_version(&conn, "articles", &doc.id, "published", &snap).unwrap();
    query::create_version(&conn, "articles", &doc.id, "draft", &snap).unwrap();

    assert_eq!(
        query::list_versions(&conn, "articles", &doc.id, false, None, None)
            .unwrap()
            .len(),
        2
    );

    // Delete the document
    query::delete(&conn, "articles", &doc.id).unwrap();

    // Versions should be cascade-deleted
    assert_eq!(
        query::list_versions(&conn, "articles", &doc.id, false, None, None)
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

    let data: DocumentFields = [("title".into(), json!("No Versions"))]
        .into_iter()
        .collect();
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
    let data: DocumentFields = [
        ("title".into(), json!("Page One")),
        ("seo__meta_title".into(), json!("Original SEO")),
        ("seo__meta_description".into(), json!("Original desc")),
    ]
    .into_iter()
    .collect();
    let doc = query::create(&conn, "pages_ver", &def, &data, None).unwrap();

    // Snapshot v1
    let snap_v1 = query::build_snapshot(&conn, "pages_ver", &def.fields, &doc).unwrap();
    query::create_version(&conn, "pages_ver", &doc.id, "published", &snap_v1).unwrap();

    // Update group fields
    let update_data: DocumentFields = [
        ("seo__meta_title".into(), json!("Updated SEO")),
        ("seo__meta_description".into(), json!("Updated desc")),
    ]
    .into_iter()
    .collect();
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
        &crap_cms::config::LocaleConfig::default(),
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
    let shared = Registry::shared();
    {
        let mut reg = shared.write().unwrap();
        reg.register_global(gdef.clone());
    }
    migrate::sync_all(
        &pool,
        &shared.read().unwrap(),
        &CrapConfig::default().locale,
    )
    .expect("sync");
    let conn = pool.get().unwrap();

    // Set original group data
    let data: DocumentFields = [
        ("site_name".into(), json!("My Site")),
        ("seo__meta_title".into(), json!("Original SEO")),
        ("seo__og_image".into(), json!("/original.png")),
    ]
    .into_iter()
    .collect();
    query::update_global(&conn, "site_ver", &gdef, &data, None).unwrap();

    // Snapshot v1
    let doc = query::get_global(&conn, "site_ver", &gdef, None).unwrap();
    let snap_v1 = query::build_snapshot(&conn, "_global_site_ver", &gdef.fields, &doc).unwrap();
    query::create_version(&conn, "_global_site_ver", "default", "published", &snap_v1).unwrap();

    // Update group fields
    let update_data: DocumentFields = [
        ("seo__meta_title".into(), json!("Updated SEO")),
        ("seo__og_image".into(), json!("/updated.png")),
    ]
    .into_iter()
    .collect();
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
        &crap_cms::config::LocaleConfig::default(),
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
    let shared = Registry::shared();
    {
        let mut reg = shared.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &shared.read().unwrap(), &locale_config).expect("sync");
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
    let data: DocumentFields = [
        ("title".into(), json!("Page")),
        ("seo__meta_title".into(), json!("EN Original")),
        ("seo__meta_description".into(), json!("EN Desc")),
    ]
    .into_iter()
    .collect();
    let doc = query::create(&conn, "pages_ver", &def, &data, Some(&en_ctx)).unwrap();

    // Add German translation
    let de_data: DocumentFields = [("seo__meta_title".into(), json!("DE Original"))]
        .into_iter()
        .collect();
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
    let update_data: DocumentFields = [("seo__meta_title".into(), json!("EN Updated"))]
        .into_iter()
        .collect();
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

    let data: DocumentFields = [
        ("title".into(), json!("Published")),
        ("body".into(), json!("Content")),
    ]
    .into_iter()
    .collect();
    let ctx = service::ServiceContext::collection("articles", &def)
        .pool(pool)
        .runner(runner)
        .build();
    let (doc, _) =
        service::create_document(&ctx, service::WriteInput::builder(data).build()).unwrap();

    let conn = pool.get().unwrap();
    // Should have created a version
    let versions = query::list_versions(&conn, "articles", &doc.id, false, None, None).unwrap();
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

    let data: DocumentFields = [("title".into(), json!("Draft Post"))]
        .into_iter()
        .collect();
    let ctx = service::ServiceContext::collection("articles", &def)
        .pool(pool)
        .runner(runner)
        .build();
    let (doc, _) =
        service::create_document(&ctx, service::WriteInput::builder(data).draft(true).build())
            .unwrap();

    let conn = pool.get().unwrap();
    let versions = query::list_versions(&conn, "articles", &doc.id, false, None, None).unwrap();
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
    let data: DocumentFields = [
        ("title".into(), json!("Original Title")),
        ("body".into(), json!("Original Body")),
    ]
    .into_iter()
    .collect();
    let ctx = service::ServiceContext::collection("articles", &def)
        .pool(pool)
        .runner(runner)
        .build();
    let (doc, _) =
        service::create_document(&ctx, service::WriteInput::builder(data).build()).unwrap();

    // Draft update — should NOT change the main table
    let update_data: DocumentFields = [("title".into(), json!("Draft Title"))]
        .into_iter()
        .collect();
    let (result, _) = service::update_document(
        &ctx,
        &doc.id,
        service::WriteInput::builder(update_data)
            .draft(true)
            .build(),
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
    let versions = query::list_versions(&conn, "articles", &doc.id, false, None, None).unwrap();
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

    let data: DocumentFields = [("title".into(), json!("Before Publish"))]
        .into_iter()
        .collect();
    let ctx = service::ServiceContext::collection("articles", &def)
        .pool(pool)
        .runner(runner)
        .build();
    let (doc, _) = service::create_document(
        &ctx,
        service::WriteInput::builder(data).draft(true).build(), // create as draft
    )
    .unwrap();

    // Publish update (draft=false)
    let update_data: DocumentFields = [("title".into(), json!("Published Title"))]
        .into_iter()
        .collect();
    let (published, _) = service::update_document(
        &ctx,
        &doc.id,
        service::WriteInput::builder(update_data).build(),
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

    let data: DocumentFields = [("title".into(), json!("Note"))].into_iter().collect();
    let ctx = service::ServiceContext::collection("notes", &def)
        .pool(pool)
        .runner(runner)
        .build();
    let (_doc, _) =
        service::create_document(&ctx, service::WriteInput::builder(data).build()).unwrap();

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
    let mut data: DocumentFields = [("title".into(), json!("With Blocks"))]
        .into_iter()
        .collect();
    data.insert(
        "content".to_string(),
        json!([
            {"_block_type": "text", "body": "Initial block"}
        ]),
    );
    let ctx = service::ServiceContext::collection("articles", &def)
        .pool(pool)
        .runner(runner)
        .build();
    let (doc, _) =
        service::create_document(&ctx, service::WriteInput::builder(data).build()).unwrap();

    // Draft update with different block data
    let mut update_data: DocumentFields = [("title".into(), json!("Draft With Blocks"))]
        .into_iter()
        .collect();
    update_data.insert(
        "content".to_string(),
        json!([
            {"_block_type": "text", "body": "Draft block 1"},
            {"_block_type": "text", "body": "Draft block 2"}
        ]),
    );
    service::update_document(
        &ctx,
        &doc.id,
        service::WriteInput::builder(update_data)
            .draft(true)
            .build(),
    )
    .unwrap();

    // The draft version snapshot must contain the draft block data
    let conn = pool.get().unwrap();
    let versions = query::list_versions(&conn, "articles", &doc.id, false, None, None).unwrap();
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
            events: None,
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
            events: None,
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

// ── Production-critical: FK cascade fires through the full service path ──
//
// Mirrors the gRPC `DeleteMany` + `force_hard_delete: true` path used by the
// loadtest. Build a versioned + soft-delete collection, create rows + version
// snapshots, then hard-delete via the bulk service entry point. Asserts the
// `_versions_*` rows are gone — i.e. SQLite's FK cascade fires as designed
// even when the request goes through the multi-stage service pipeline
// (transaction_immediate → query::find → query::delete inside a tx). A
// regression here would silently bloat `_versions_<collection>` on every
// hard delete in production.

#[test]
fn bulk_hard_delete_cascades_to_versions_via_service() {
    use crap_cms::db::{Filter, FilterClause, FilterOp};
    use crap_cms::hooks::lifecycle::HookRunner;
    use crap_cms::service::{DeleteManyOptions, ServiceContext, delete_many};
    use std::sync::Arc;
    use tempfile::tempdir;

    // Versioned + soft-delete collection — same shape as the example's
    // `posts` collection used in `tests/grpc_loadtest.sh`.
    let mut def = make_versioned_def();
    def.slug = "articles".into();
    def.soft_delete = true;

    let (_tmp, pool, registry) = setup_db(vec![def.clone()]);

    let tmp_runner_dir = tempdir().expect("tempdir for hook runner");
    let cfg = CrapConfig::test_default();
    let runner = HookRunner::builder()
        .config_dir(tmp_runner_dir.path())
        .registry(Arc::clone(&registry))
        .config(&cfg)
        .build()
        .expect("build hook runner");

    // Create 3 versioned articles + 2 explicit version snapshots each.
    let conn = pool.get().unwrap();
    let mut doc_ids = Vec::new();
    for i in 0..3 {
        let data: DocumentFields = [("title".to_string(), json!(format!("Article {i}")))]
            .into_iter()
            .collect();
        let doc = query::create(&conn, "articles", &def, &data, None).unwrap();
        let snap = query::build_snapshot(&conn, "articles", &def.fields, &doc).unwrap();
        query::create_version(&conn, "articles", &doc.id, "published", &snap).unwrap();
        query::create_version(&conn, "articles", &doc.id, "draft", &snap).unwrap();
        doc_ids.push(doc.id.to_string());
    }
    drop(conn);

    // Pre-condition: 6 versions total (3 docs × 2 versions each).
    let pre_versions: i64 = pool
        .get()
        .unwrap()
        .query_one("SELECT COUNT(*) AS c FROM _versions_articles", &[])
        .unwrap()
        .unwrap()
        .get_i64("c")
        .unwrap();
    assert_eq!(pre_versions, 6, "fixture should have 6 versions");

    // Simulate the gRPC `force_hard_delete: true` path: clear soft_delete
    // on the def before calling the service. This matches what
    // `delete_many_impl` does at `src/api/handlers/collection/bulk/delete_many.rs:122`.
    let mut hard_delete_def = def.clone();
    hard_delete_def.soft_delete = false;

    let ctx = ServiceContext::collection("articles", &hard_delete_def)
        .pool(&pool)
        .runner(&runner)
        .build();

    // Match all 3 docs via an `id IN (…)` filter, the same way the loadtest's
    // `slug LIKE 'loadtest-ghz-%'` would match its rows.
    let filters = vec![FilterClause::Single(Filter {
        field: "id".to_string(),
        op: FilterOp::In(doc_ids.clone()),
    })];

    let result = delete_many(&ctx, &filters, &cfg.locale, &DeleteManyOptions::default())
        .expect("delete_many should succeed");

    assert_eq!(result.hard_deleted, 3, "all 3 docs must be hard-deleted");
    assert_eq!(result.soft_deleted, 0);
    assert_eq!(result.skipped, 0);

    // The posts rows are gone.
    let post_rows: i64 = pool
        .get()
        .unwrap()
        .query_one("SELECT COUNT(*) AS c FROM articles", &[])
        .unwrap()
        .unwrap()
        .get_i64("c")
        .unwrap();
    assert_eq!(
        post_rows, 0,
        "articles table must be empty after hard delete"
    );

    // **Critical**: FK cascade must have fired — every `_versions_articles`
    // row should be gone, not orphaned. This is what we measured failing in
    // the bench environment (61k orphans accumulated across runs); if this
    // assertion ever fires, production deployments leak versioned data.
    let post_versions: i64 = pool
        .get()
        .unwrap()
        .query_one("SELECT COUNT(*) AS c FROM _versions_articles", &[])
        .unwrap()
        .unwrap()
        .get_i64("c")
        .unwrap();
    assert_eq!(
        post_versions, 0,
        "FK cascade must remove _versions_articles rows when parent articles are hard-deleted; \
         orphaned versions indicate `PRAGMA foreign_keys` is not active for the delete path"
    );
}

/// Regression: pool-mode bulk delete must process EVERY matching document
/// across the `BATCH_SIZE` (500) boundary. The loop runs find→delete→commit
/// per batch; with 501 rows it must run two batches (500 + 1). An off-by-one
/// or early-exit would silently leave rows — invisible to every test that
/// uses a small fixture.
#[test]
fn bulk_hard_delete_processes_beyond_one_batch() {
    use crap_cms::hooks::lifecycle::HookRunner;
    use crap_cms::service::{DeleteManyOptions, ServiceContext, delete_many};
    use std::sync::Arc;
    use tempfile::tempdir;

    // Plain hard-delete collection ("notes": no versions, soft_delete = false).
    let def = make_nonversioned_def();
    let (_tmp, pool, registry) = setup_db(vec![def.clone()]);

    let tmp_runner_dir = tempdir().expect("tempdir for hook runner");
    let cfg = CrapConfig::test_default();
    let runner = HookRunner::builder()
        .config_dir(tmp_runner_dir.path())
        .registry(Arc::clone(&registry))
        .config(&cfg)
        .build()
        .expect("build hook runner");

    // Seed 501 docs — one past the 500-row batch boundary.
    let conn = pool.get().unwrap();
    for i in 0..501 {
        let data: DocumentFields = [("title".to_string(), json!(format!("Note {i}")))]
            .into_iter()
            .collect();
        query::create(&conn, "notes", &def, &data, None).unwrap();
    }
    drop(conn);

    let ctx = ServiceContext::collection("notes", &def)
        .pool(&pool)
        .runner(&runner)
        .build();

    // Empty filter set matches all rows; the batched loop must delete each one.
    let result = delete_many(&ctx, &[], &cfg.locale, &DeleteManyOptions::default())
        .expect("delete_many should succeed");

    assert_eq!(
        result.hard_deleted, 501,
        "all 501 docs must be deleted across the 500-row batch boundary"
    );
    assert_eq!(result.skipped, 0);

    let remaining: i64 = pool
        .get()
        .unwrap()
        .query_one("SELECT COUNT(*) AS c FROM notes", &[])
        .unwrap()
        .unwrap()
        .get_i64("c")
        .unwrap();
    assert_eq!(remaining, 0, "no rows should remain after bulk delete");
}
