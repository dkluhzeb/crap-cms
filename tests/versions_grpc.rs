//! Integration tests for the versioning and drafts system.
//!
//! Covers: DB query layer (create/list/find/restore/prune versions, status),
//! service layer (create_document/update_document with draft param),
//! and gRPC API (draft flag on CRUD RPCs, ListVersions, RestoreVersion).

use std::collections::HashMap;
use std::sync::Arc;

use prost_types::{Struct, Value, value::Kind};
use tonic::Request;

use crap_cms::api::content;
use crap_cms::api::content::content_api_server::ContentApi;
use crap_cms::api::handlers::{ContentService, ContentServiceDeps};
use crap_cms::config::*;
use crap_cms::core::Registry;
use crap_cms::core::collection::*;
use crap_cms::core::email::EmailRenderer;
use crap_cms::core::field::*;
use crap_cms::db::{migrate, pool, query};
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

fn make_versioned_no_drafts_def() -> CollectionDefinition {
    let mut def = make_versioned_def();
    def.slug = "docs".into();
    def.versions = Some(VersionsConfig::new(false, 5));
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
    _pool: crap_cms::db::DbPool,
    _registry: crap_cms::core::SharedRegistry,
    _runner: HookRunner,
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
        _pool: db_pool,
        _registry: registry,
        _runner: hook_runner,
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

#[tokio::test]
async fn grpc_find_where_published_status() {
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
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "articles".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 1);
    assert_eq!(
        get_proto_field(&resp.documents[0], "title").as_deref(),
        Some("Published")
    );
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
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "articles".to_string(),
            draft: Some(true),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 2);
}

#[tokio::test]
async fn grpc_draft_update_is_version_only() {
    let ts = setup_service(vec![make_versioned_def()]);

    // Create published article
    let doc = ts
        .service
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
            unpublish: None,
        }))
        .await
        .unwrap();

    // Regular find should still show "Original" (main table unchanged)
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "articles".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.documents.len(), 1);
    assert_eq!(
        get_proto_field(&resp.documents[0], "title").as_deref(),
        Some("Original")
    );
}

#[tokio::test]
async fn grpc_find_by_id_draft_returns_latest_version() {
    let ts = setup_service(vec![make_versioned_def()]);

    // Create published article
    let doc = ts
        .service
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
            unpublish: None,
        }))
        .await
        .unwrap();

    // FindByID with draft=true should load the draft version
    let resp = ts
        .service
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
    assert_eq!(
        get_proto_field(&found, "title").as_deref(),
        Some("Draft Title")
    );
}

#[tokio::test]
async fn grpc_find_by_id_no_draft_returns_main_table() {
    let ts = setup_service(vec![make_versioned_def()]);

    let doc = ts
        .service
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
            unpublish: None,
        }))
        .await
        .unwrap();

    // FindByID without draft should return main table data
    let resp = ts
        .service
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
    assert_eq!(
        get_proto_field(&found, "title").as_deref(),
        Some("Main Table")
    );
}

#[tokio::test]
async fn grpc_list_versions() {
    let ts = setup_service(vec![make_versioned_def()]);

    let doc = ts
        .service
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
                unpublish: None,
            }))
            .await
            .unwrap();
    }

    let resp = ts
        .service
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

    let doc = ts
        .service
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
                unpublish: None,
            }))
            .await
            .unwrap();
    }

    let resp = ts
        .service
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

    let doc = ts
        .service
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

    let err = ts
        .service
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
    let doc = ts
        .service
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
    let v1_list = ts
        .service
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
            unpublish: None,
        }))
        .await
        .unwrap();

    // Verify current title is Version 2
    let current = ts
        .service
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
    assert_eq!(
        get_proto_field(&current, "title").as_deref(),
        Some("Version 2")
    );

    // Restore version 1
    let restored = ts
        .service
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

    assert_eq!(
        get_proto_field(&restored, "title").as_deref(),
        Some("Version 1")
    );

    // Should now have 3 versions (original + update + restore)
    let versions = ts
        .service
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

    let err = ts
        .service
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
    let resp = ts
        .service
        .describe_collection(Request::new(content::DescribeCollectionRequest {
            slug: "articles".to_string(),
            is_global: false,
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(resp.drafts, "articles should have drafts=true");

    // Non-versioned
    let resp = ts
        .service
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
    let result = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "articles".to_string(),
            data: Some(make_struct(&[("body", "Just a body, no title")])),
            locale: None,
            draft: Some(true),
        }))
        .await;

    // Should succeed because draft skips required
    assert!(
        result.is_ok(),
        "Draft create should skip required validation"
    );
}

#[tokio::test]
async fn grpc_publish_enforces_required_validation() {
    let ts = setup_service(vec![make_versioned_def()]);

    // Create without required title and draft=false (publish)
    let result = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "articles".to_string(),
            data: Some(make_struct(&[("body", "No title")])),
            locale: None,
            draft: Some(false),
        }))
        .await;

    // Should fail because title is required for publish
    assert!(
        result.is_err(),
        "Publish should enforce required validation"
    );
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
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "docs".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 2);
}

#[tokio::test]
async fn grpc_versioned_no_drafts_still_creates_versions() {
    let ts = setup_service(vec![make_versioned_no_drafts_def()]);

    let doc = ts
        .service
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

    let resp = ts
        .service
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

    let doc = ts
        .service
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
                unpublish: None,
            }))
            .await
            .unwrap();
    }

    // max_versions=5, so only 5 should remain
    let resp = ts
        .service
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
    let doc = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "articles".to_string(),
            data: Some(make_struct(&[
                ("title", "New Article"),
                ("body", "WIP content"),
            ])),
            locale: None,
            draft: Some(true),
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // 2. Should not be in regular find
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "articles".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        resp.pagination.as_ref().unwrap().total_docs,
        0,
        "draft should not appear in regular find"
    );

    // 3. Should appear with draft=true
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "articles".to_string(),
            draft: Some(true),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        resp.pagination.as_ref().unwrap().total_docs,
        1,
        "draft should appear with draft=true"
    );

    // 4. Make a draft update
    ts.service
        .update(Request::new(content::UpdateRequest {
            collection: "articles".to_string(),
            id: doc.id.clone(),
            data: Some(make_struct(&[
                ("title", "Revised Article"),
                ("body", "Better content"),
            ])),
            locale: None,
            draft: Some(true),
            unpublish: None,
        }))
        .await
        .unwrap();

    // 5. Publish the article
    ts.service
        .update(Request::new(content::UpdateRequest {
            collection: "articles".to_string(),
            id: doc.id.clone(),
            data: Some(make_struct(&[
                ("title", "Final Article"),
                ("body", "Final content"),
            ])),
            locale: None,
            draft: Some(false),
            unpublish: None,
        }))
        .await
        .unwrap();

    // 6. Should now appear in regular find
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "articles".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 1);
    assert_eq!(
        get_proto_field(&resp.documents[0], "title").as_deref(),
        Some("Final Article")
    );

    // 7. Should have 3 versions (create draft + draft update + publish)
    let versions = ts
        .service
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

// ── gRPC Unpublish ───────────────────────────────────────────────────────────

#[tokio::test]
async fn grpc_update_unpublish() {
    let ts = setup_service(vec![make_versioned_def()]);

    // 1. Create a published document
    let doc = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "articles".to_string(),
            data: Some(make_struct(&[
                ("title", "To Unpublish"),
                ("body", "Published content"),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // Verify it's published (appears in regular find)
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "articles".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        resp.pagination.as_ref().unwrap().total_docs,
        1,
        "should be visible as published"
    );

    // 2. Unpublish via update with unpublish=true
    let unpublished = ts
        .service
        .update(Request::new(content::UpdateRequest {
            collection: "articles".to_string(),
            id: doc.id.clone(),
            data: None,
            locale: None,
            draft: None,
            unpublish: Some(true),
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();
    assert_eq!(unpublished.id, doc.id);

    // 3. Should NOT appear in regular find (status is now "draft")
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "articles".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        resp.pagination.as_ref().unwrap().total_docs,
        0,
        "unpublished doc should not appear in regular find"
    );

    // 4. Should appear with draft=true
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "articles".to_string(),
            draft: Some(true),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        resp.pagination.as_ref().unwrap().total_docs,
        1,
        "unpublished doc should appear with draft=true"
    );

    // 5. Verify a draft version was created
    let versions = ts
        .service
        .list_versions(Request::new(content::ListVersionsRequest {
            collection: "articles".to_string(),
            id: doc.id.clone(),
            limit: None,
        }))
        .await
        .unwrap()
        .into_inner();
    // Should have 2 versions: initial published + unpublish draft
    assert!(
        versions.versions.len() >= 2,
        "should have at least 2 versions, got {}",
        versions.versions.len()
    );
    assert_eq!(
        versions.versions[0].status, "draft",
        "latest version should be draft"
    );
}

// ── persist_* Direct Tests ────────────────────────────────────────────────

#[test]
fn persist_create_published() {
    let def = make_versioned_def();
    let (_tmp, pool, _registry) = setup_db(vec![def.clone()]);
    let conn = pool.get().unwrap();

    let final_data: HashMap<String, String> = [
        ("title".into(), "Persist Published".into()),
        ("body".into(), "Content".into()),
    ]
    .into();
    let hook_data: HashMap<String, serde_json::Value> = final_data
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();

    let doc = service::persist_create(
        &conn,
        "articles",
        &def,
        &final_data,
        &hook_data,
        &service::PersistOptions::default(),
    )
    .unwrap();
    assert_eq!(doc.get_str("title"), Some("Persist Published"));

    // Document should exist in main table
    let found = query::find_by_id(&conn, "articles", &def, &doc.id, None).unwrap();
    assert!(found.is_some());

    // Version snapshot should exist with status "published"
    let versions = query::list_versions(&conn, "articles", &doc.id, None, None).unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].status, "published");

    // Status column should be published
    let status = query::get_document_status(&conn, "articles", &doc.id).unwrap();
    assert_eq!(status.as_deref(), Some("published"));
}

#[test]
fn persist_create_draft() {
    let def = make_versioned_def();
    let (_tmp, pool, _registry) = setup_db(vec![def.clone()]);
    let conn = pool.get().unwrap();

    let final_data: HashMap<String, String> = [("title".into(), "Persist Draft".into())].into();
    let hook_data: HashMap<String, serde_json::Value> = final_data
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();

    let opts = service::PersistOptions::builder().draft(true).build();
    let doc =
        service::persist_create(&conn, "articles", &def, &final_data, &hook_data, &opts).unwrap();

    // Document should exist with _status = "draft"
    let status = query::get_document_status(&conn, "articles", &doc.id).unwrap();
    assert_eq!(status.as_deref(), Some("draft"));

    // Version snapshot should exist with status "draft"
    let versions = query::list_versions(&conn, "articles", &doc.id, None, None).unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].status, "draft");
}

#[test]
fn persist_update_publishes() {
    let def = make_versioned_def();
    let (_tmp, pool, _registry) = setup_db(vec![def.clone()]);
    let conn = pool.get().unwrap();

    // Create a document first
    let create_data: HashMap<String, String> = [("title".into(), "Before Update".into())].into();
    let doc = query::create(&conn, "articles", &def, &create_data, None).unwrap();

    // Now use persist_update
    let update_data: HashMap<String, String> = [
        ("title".into(), "After Update".into()),
        ("body".into(), "New body".into()),
    ]
    .into();
    let hook_data: HashMap<String, serde_json::Value> = update_data
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();

    let updated = service::persist_update(
        &conn,
        "articles",
        &doc.id,
        &def,
        &update_data,
        &hook_data,
        &service::PersistOptions::default(),
    )
    .unwrap();
    assert_eq!(updated.get_str("title"), Some("After Update"));

    // Main table should be updated
    let found = query::find_by_id(&conn, "articles", &def, &doc.id, None)
        .unwrap()
        .unwrap();
    assert_eq!(found.get_str("title"), Some("After Update"));

    // Version snapshot should exist with status "published"
    let versions = query::list_versions(&conn, "articles", &doc.id, None, None).unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].status, "published");
}

#[test]
fn persist_draft_version_merges_data() {
    let def = make_versioned_def();
    let (_tmp, pool, _registry) = setup_db(vec![def.clone()]);
    let conn = pool.get().unwrap();

    // Create a published document
    let create_data: HashMap<String, String> = [
        ("title".into(), "Original".into()),
        ("body".into(), "Original body".into()),
    ]
    .into();
    let doc = query::create(&conn, "articles", &def, &create_data, None).unwrap();

    // Call persist_draft_version with modified hook data
    let hook_data: HashMap<String, serde_json::Value> = [(
        "title".to_string(),
        serde_json::Value::String("Draft Title".to_string()),
    )]
    .into();

    let existing =
        service::persist_draft_version(&conn, "articles", &doc.id, &def, &hook_data, None).unwrap();

    // Returned doc is the existing (unchanged) doc
    assert_eq!(existing.get_str("title"), Some("Original"));

    // Main table should still have original data
    let main = query::find_by_id(&conn, "articles", &def, &doc.id, None)
        .unwrap()
        .unwrap();
    assert_eq!(main.get_str("title"), Some("Original"));

    // Draft version snapshot should have the merged "Draft Title"
    let versions = query::list_versions(&conn, "articles", &doc.id, None, None).unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].status, "draft");
    assert_eq!(
        versions[0].snapshot.get("title").and_then(|v| v.as_str()),
        Some("Draft Title"),
        "draft snapshot should contain merged title"
    );
    // Body should be carried from existing doc
    assert_eq!(
        versions[0].snapshot.get("body").and_then(|v| v.as_str()),
        Some("Original body"),
        "draft snapshot should carry existing body"
    );
}

#[test]
fn persist_unpublish_sets_draft_status() {
    let def = make_versioned_def();
    let (_tmp, pool, _registry) = setup_db(vec![def.clone()]);
    let conn = pool.get().unwrap();

    // Create a published document
    let create_data: HashMap<String, String> = [("title".into(), "To Unpublish".into())].into();
    let doc = query::create(&conn, "articles", &def, &create_data, None).unwrap();
    query::set_document_status(&conn, "articles", &doc.id, "published").unwrap();

    // Call persist_unpublish
    let result = service::persist_unpublish(&conn, "articles", &doc.id, &def).unwrap();
    assert_eq!(result.get_str("title"), Some("To Unpublish"));

    // Status should now be "draft"
    let status = query::get_document_status(&conn, "articles", &doc.id).unwrap();
    assert_eq!(status.as_deref(), Some("draft"));

    // A draft version snapshot should have been created
    let versions = query::list_versions(&conn, "articles", &doc.id, None, None).unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].status, "draft");
}

// ── Locale Regression Tests ──────────────────────────────────────────────

/// Regression: persist_draft_version must receive the caller's locale_ctx
/// so that find_by_id_raw reads locale-resolved columns. Previously `None`
/// was always passed, causing the wrong column values to be read for
/// locale-specific draft saves.
#[test]
fn service_update_draft_uses_locale_context() {
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
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.locale = locale_config.clone();

    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");
    let registry = Registry::shared();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&db_pool, &registry, &locale_config).expect("sync");

    let hook_runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(registry.clone())
        .config(&config)
        .build()
        .expect("hook runner");

    // Locale contexts
    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config.clone(),
    };

    // 1. Create a published document with EN title
    let data: HashMap<String, String> = [
        ("title".into(), "English Title".into()),
        ("body".into(), "Body".into()),
    ]
    .into();
    let (doc, _) = service::create_document(
        &db_pool,
        &hook_runner,
        "articles",
        &def,
        service::WriteInput::builder(data, &HashMap::new())
            .locale_ctx(Some(&en_ctx))
            .locale(Some("en".to_string()))
            .build(),
        None,
        false,
    )
    .unwrap();

    // 2. Add German translation via direct query
    let de_data: HashMap<String, String> = [("title".into(), "Deutscher Titel".into())].into();
    {
        let conn = db_pool.get().unwrap();
        query::update(&conn, "articles", &def, &doc.id, &de_data, Some(&de_ctx)).unwrap();
    }

    // 3. Draft update with DE locale, changing the German title
    let draft_data: HashMap<String, String> =
        [("title".into(), "Neuer Deutscher Titel".into())].into();
    let (result, _) = service::update_document(
        &db_pool,
        &hook_runner,
        "articles",
        &doc.id,
        &def,
        service::WriteInput::builder(draft_data, &HashMap::new())
            .locale_ctx(Some(&de_ctx))
            .locale(Some("de".to_string()))
            .draft(true)
            .build(),
        None,
        false,
    )
    .unwrap();

    // Result should be the existing doc (main table unchanged by draft)
    // With the fix, persist_draft_version reads with DE locale context,
    // so it sees the German title
    assert_eq!(result.get_str("title"), Some("Deutscher Titel"));

    // 4. Main table EN title should be unchanged
    {
        let conn = db_pool.get().unwrap();
        let en_doc = query::find_by_id(&conn, "articles", &def, &doc.id, Some(&en_ctx))
            .unwrap()
            .unwrap();
        assert_eq!(
            en_doc.get_str("title"),
            Some("English Title"),
            "EN title should be unchanged"
        );
    }

    // 5. The draft version snapshot should have the new DE title
    {
        let conn = db_pool.get().unwrap();
        let versions = query::list_versions(&conn, "articles", &doc.id, None, None).unwrap();
        // Should have 2 versions: initial published create + draft update
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].status, "draft");
        assert_eq!(
            versions[0].snapshot.get("title").and_then(|v| v.as_str()),
            Some("Neuer Deutscher Titel"),
            "draft snapshot should contain the updated DE title"
        );
    }
}
