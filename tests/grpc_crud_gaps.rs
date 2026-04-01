//! Localization, drafts, versions, complex globals, has-many relationships,
//! bulk operations, count, FTS search, and jobs RPC tests.
//!
//! Uses ContentService directly (no network) via ContentApi trait.

use std::collections::BTreeMap;
use std::sync::Arc;

use prost_types::{Struct, Value, value::Kind};
use tonic::Request;

use crap_cms::api::content;
use crap_cms::api::content::content_api_server::ContentApi;
use crap_cms::api::service::{ContentService, ContentServiceDeps};
use crap_cms::config::*;
use crap_cms::core::Registry;
use crap_cms::core::collection::*;
use crap_cms::core::email::EmailRenderer;
use crap_cms::core::field::*;
use crap_cms::db::{migrate, pool};
use crap_cms::hooks::lifecycle::HookRunner;
use serde_json::json;

// ── Helpers ───────────────────────────────────────────────────────────────

fn make_posts_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("status", FieldType::Select)
            .default_value(json!("draft"))
            .build(),
    ];
    def
}

fn make_users_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("users");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("User".to_string())),
        plural: Some(LocalizedString::Plain("Users".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("email", FieldType::Email)
            .required(true)
            .unique(true)
            .build(),
        FieldDefinition::builder("name", FieldType::Text).build(),
    ];
    def.auth = Some(Auth {
        enabled: true,
        ..Default::default()
    });
    def
}

/// Build a prost Struct from key-value string pairs.
fn make_struct(pairs: &[(&str, &str)]) -> Struct {
    let mut fields = BTreeMap::new();
    for (k, v) in pairs {
        fields.insert(
            k.to_string(),
            Value {
                kind: Some(Kind::StringValue(v.to_string())),
            },
        );
    }
    Struct { fields }
}

/// Extract a string field from a proto Document's fields struct.
fn get_proto_field(doc: &content::Document, field: &str) -> Option<String> {
    doc.fields.as_ref().and_then(|s| {
        s.fields.get(field).and_then(|v| match &v.kind {
            Some(Kind::StringValue(s)) => Some(s.clone()),
            _ => None,
        })
    })
}

struct TestSetup {
    _tmp: tempfile::TempDir,
    service: ContentService,
    #[allow(dead_code)]
    pool: crap_cms::db::DbPool,
}

fn setup_service(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
) -> TestSetup {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();

    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");

    let registry = Registry::shared();
    {
        let mut reg = registry.write().unwrap();
        for def in &collections {
            reg.register_collection(def.clone());
        }
        for def in &globals {
            reg.register_global(def.clone());
        }
    }

    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync schema");

    let hook_runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(registry.clone())
        .config(&config)
        .build()
        .expect("create hook runner");

    let email_renderer = Arc::new(EmailRenderer::new(tmp.path()).expect("create email renderer"));

    let service = ContentService::new(
        ContentServiceDeps::builder()
            .pool(db_pool.clone())
            .registry(Registry::snapshot(&registry))
            .hook_runner(hook_runner)
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
            .build(),
    );

    TestSetup {
        _tmp: tmp,
        service,
        pool: db_pool,
    }
}

fn make_versioned_posts_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("body", FieldType::Textarea).build(),
    ];
    def.versions = Some(VersionsConfig::new(true, 10));
    def
}

// ── Group 6: Localization (gRPC) ──────────────────────────────────────────

#[tokio::test]
async fn list_and_restore_versions() {
    let ts = setup_service(vec![make_versioned_posts_def()], vec![]);

    // Create a published post
    let doc = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(make_struct(&[
                ("title", "Version 1"),
                ("body", "First version"),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // Update to version 2
    ts.service
        .update(Request::new(content::UpdateRequest {
            collection: "posts".to_string(),
            id: doc.id.clone(),
            data: Some(make_struct(&[
                ("title", "Version 2"),
                ("body", "Second version"),
            ])),
            locale: None,
            draft: None,
            unpublish: None,
        }))
        .await
        .unwrap();

    // Update to version 3
    ts.service
        .update(Request::new(content::UpdateRequest {
            collection: "posts".to_string(),
            id: doc.id.clone(),
            data: Some(make_struct(&[
                ("title", "Version 3"),
                ("body", "Third version"),
            ])),
            locale: None,
            draft: None,
            unpublish: None,
        }))
        .await
        .unwrap();

    // List versions
    let versions_resp = ts
        .service
        .list_versions(Request::new(content::ListVersionsRequest {
            collection: "posts".to_string(),
            id: doc.id.clone(),
            limit: None,
        }))
        .await
        .unwrap()
        .into_inner();

    assert!(
        versions_resp.versions.len() >= 2,
        "Should have at least 2 versions, got {}",
        versions_resp.versions.len()
    );

    // The latest version should be at the top
    let latest = &versions_resp.versions[0];
    assert!(latest.latest, "First version in list should be latest");

    // Restore an earlier version (not the latest)
    let earlier = versions_resp
        .versions
        .iter()
        .find(|v| !v.latest)
        .expect("Should have a non-latest version");

    let restored = ts
        .service
        .restore_version(Request::new(content::RestoreVersionRequest {
            collection: "posts".to_string(),
            document_id: doc.id.clone(),
            version_id: earlier.id.clone(),
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // The restored document should have an earlier version's title
    let restored_title = get_proto_field(&restored, "title");
    assert!(restored_title.is_some(), "Restored doc should have a title");
    assert_ne!(
        restored_title.as_deref(),
        Some("Version 3"),
        "Restored should not be version 3 anymore"
    );
}

// ── Access Control / CRUD Gaps ────────────────────────────────────────────

#[tokio::test]
async fn create_returns_document_with_fields() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    // Create a document
    let doc = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(make_struct(&[
                ("title", "Field Check"),
                ("status", "draft"),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // Verify the create response has all fields
    assert!(!doc.id.is_empty(), "Document should have an ID");
    assert_eq!(doc.collection, "posts");
    assert_eq!(
        get_proto_field(&doc, "title").as_deref(),
        Some("Field Check")
    );
    assert_eq!(get_proto_field(&doc, "status").as_deref(), Some("draft"));

    // Also fetch via FindByID with depth=0 to verify persistence
    let found = ts
        .service
        .find_by_id(Request::new(content::FindByIdRequest {
            collection: "posts".to_string(),
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
        .expect("Document should be found");

    assert_eq!(found.id, doc.id);
    assert_eq!(
        get_proto_field(&found, "title").as_deref(),
        Some("Field Check")
    );
    assert_eq!(get_proto_field(&found, "status").as_deref(), Some("draft"));
}

#[tokio::test]
async fn find_with_pagination() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    // Create 5 documents with ordered titles
    for i in 0..5 {
        ts.service
            .create(Request::new(content::CreateRequest {
                collection: "posts".to_string(),
                data: Some(make_struct(&[("title", &format!("Page {}", i))])),
                locale: None,
                draft: None,
            }))
            .await
            .unwrap();
    }

    // Find with limit=2, page=2 — should return the 3rd and 4th documents
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "posts".to_string(),
            limit: Some(2),
            page: Some(2),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.documents.len(), 2, "Should return exactly 2 documents");
    assert_eq!(
        resp.pagination.as_ref().unwrap().total_docs,
        5,
        "Total count should still be 5 regardless of pagination"
    );

    // Verify we can get the remaining page (page 3)
    let resp2 = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "posts".to_string(),
            limit: Some(2),
            page: Some(3),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp2.documents.len(), 1, "Last page should have 1 document");
    assert_eq!(
        resp2.pagination.as_ref().unwrap().total_docs,
        5,
        "Total count should still be 5"
    );
}

// ══════════════════════════════════════════════════════════════════════════════
// Count RPC Tests
// ══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn count_empty_collection() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let resp = ts
        .service
        .count(Request::new(content::CountRequest {
            collection: "posts".to_string(),
            ..Default::default()
        }))
        .await
        .expect("Count failed");

    assert_eq!(resp.into_inner().count, 0);
}

#[tokio::test]
async fn count_with_documents() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    for title in &["A", "B", "C"] {
        ts.service
            .create(Request::new(content::CreateRequest {
                collection: "posts".to_string(),
                data: Some(make_struct(&[("title", title)])),
                locale: None,
                draft: None,
            }))
            .await
            .unwrap();
    }

    let resp = ts
        .service
        .count(Request::new(content::CountRequest {
            collection: "posts".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.count, 3);
}

#[tokio::test]
async fn count_with_where() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    for (title, status) in &[("A", "draft"), ("B", "published"), ("C", "published")] {
        ts.service
            .create(Request::new(content::CreateRequest {
                collection: "posts".to_string(),
                data: Some(make_struct(&[("title", title), ("status", status)])),
                locale: None,
                draft: None,
            }))
            .await
            .unwrap();
    }

    let resp = ts
        .service
        .count(Request::new(content::CountRequest {
            collection: "posts".to_string(),
            r#where: Some(r#"{"status": "published"}"#.to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.count, 2);
}

#[tokio::test]
async fn count_with_where_json() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    for (title, status) in &[("A", "draft"), ("B", "published")] {
        ts.service
            .create(Request::new(content::CreateRequest {
                collection: "posts".to_string(),
                data: Some(make_struct(&[("title", title), ("status", status)])),
                locale: None,
                draft: None,
            }))
            .await
            .unwrap();
    }

    let resp = ts
        .service
        .count(Request::new(content::CountRequest {
            collection: "posts".to_string(),
            r#where: Some(r#"{"status": "draft"}"#.to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.count, 1);
}

#[tokio::test]
async fn count_nonexistent_collection() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let err = ts
        .service
        .count(Request::new(content::CountRequest {
            collection: "nonexistent".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::NotFound);
}

// ══════════════════════════════════════════════════════════════════════════════
// UpdateMany / DeleteMany Tests
// ══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn update_many_basic() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    for title in &["X", "Y", "Z"] {
        ts.service
            .create(Request::new(content::CreateRequest {
                collection: "posts".to_string(),
                data: Some(make_struct(&[("title", title), ("status", "draft")])),
                locale: None,
                draft: None,
            }))
            .await
            .unwrap();
    }

    let resp = ts
        .service
        .update_many(Request::new(content::UpdateManyRequest {
            collection: "posts".to_string(),
            r#where: Some(r#"{"status": "draft"}"#.to_string()),
            data: Some(make_struct(&[("status", "published")])),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.modified, 3);
}

#[tokio::test]
async fn update_many_with_where_partial() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    for (title, status) in &[("A", "draft"), ("B", "published"), ("C", "draft")] {
        ts.service
            .create(Request::new(content::CreateRequest {
                collection: "posts".to_string(),
                data: Some(make_struct(&[("title", title), ("status", status)])),
                locale: None,
                draft: None,
            }))
            .await
            .unwrap();
    }

    let resp = ts
        .service
        .update_many(Request::new(content::UpdateManyRequest {
            collection: "posts".to_string(),
            r#where: Some(r#"{"status": "draft"}"#.to_string()),
            data: Some(make_struct(&[("status", "published")])),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.modified, 2);
}

#[tokio::test]
async fn update_many_no_matches() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let resp = ts
        .service
        .update_many(Request::new(content::UpdateManyRequest {
            collection: "posts".to_string(),
            r#where: Some(r#"{"status": "nonexistent"}"#.to_string()),
            data: Some(make_struct(&[("status", "published")])),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.modified, 0);
}

#[tokio::test]
async fn delete_many_basic() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    for title in &["A", "B", "C"] {
        ts.service
            .create(Request::new(content::CreateRequest {
                collection: "posts".to_string(),
                data: Some(make_struct(&[("title", title), ("status", "draft")])),
                locale: None,
                draft: None,
            }))
            .await
            .unwrap();
    }

    let resp = ts
        .service
        .delete_many(Request::new(content::DeleteManyRequest {
            collection: "posts".to_string(),
            r#where: Some(r#"{"status": "draft"}"#.to_string()),
            hooks: None,
            force_hard_delete: false,
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.deleted, 3);

    // Verify all deleted
    let count = ts
        .service
        .count(Request::new(content::CountRequest {
            collection: "posts".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .count;
    assert_eq!(count, 0);
}

#[tokio::test]
async fn delete_many_with_where_partial() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    for (title, status) in &[("A", "draft"), ("B", "published"), ("C", "draft")] {
        ts.service
            .create(Request::new(content::CreateRequest {
                collection: "posts".to_string(),
                data: Some(make_struct(&[("title", title), ("status", status)])),
                locale: None,
                draft: None,
            }))
            .await
            .unwrap();
    }

    let resp = ts
        .service
        .delete_many(Request::new(content::DeleteManyRequest {
            collection: "posts".to_string(),
            r#where: Some(r#"{"status": "draft"}"#.to_string()),
            hooks: None,
            force_hard_delete: false,
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.deleted, 2);
}

#[tokio::test]
async fn delete_many_no_matches() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let resp = ts
        .service
        .delete_many(Request::new(content::DeleteManyRequest {
            collection: "posts".to_string(),
            r#where: Some(r#"{"status": "nonexistent"}"#.to_string()),
            hooks: None,
            force_hard_delete: false,
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.deleted, 0);
}

fn make_soft_delete_posts_def() -> CollectionDefinition {
    let mut def = make_posts_def();
    def.soft_delete = true;
    def
}

#[tokio::test]
async fn delete_many_soft_deletes_when_collection_has_soft_delete() {
    let ts = setup_service(vec![make_soft_delete_posts_def()], vec![]);

    // Create 3 documents
    for title in &["A", "B", "C"] {
        ts.service
            .create(Request::new(content::CreateRequest {
                collection: "posts".to_string(),
                data: Some(make_struct(&[("title", title), ("status", "draft")])),
                locale: None,
                draft: None,
            }))
            .await
            .unwrap();
    }

    // Delete all — should soft-delete, not hard-delete
    let resp = ts
        .service
        .delete_many(Request::new(content::DeleteManyRequest {
            collection: "posts".to_string(),
            r#where: None,
            hooks: None,
            force_hard_delete: false,
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.deleted, 0, "no documents should be hard-deleted");
    assert_eq!(resp.soft_deleted, 3, "all 3 should be soft-deleted");

    // Documents should no longer appear in normal find
    let count = ts
        .service
        .count(Request::new(content::CountRequest {
            collection: "posts".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .count;
    assert_eq!(count, 0, "soft-deleted docs should not appear in count");
}

#[tokio::test]
async fn delete_many_force_hard_delete_on_soft_delete_collection() {
    let ts = setup_service(vec![make_soft_delete_posts_def()], vec![]);

    // Create 2 documents
    for title in &["X", "Y"] {
        ts.service
            .create(Request::new(content::CreateRequest {
                collection: "posts".to_string(),
                data: Some(make_struct(&[("title", title), ("status", "draft")])),
                locale: None,
                draft: None,
            }))
            .await
            .unwrap();
    }

    // Force hard-delete
    let resp = ts
        .service
        .delete_many(Request::new(content::DeleteManyRequest {
            collection: "posts".to_string(),
            r#where: None,
            hooks: None,
            force_hard_delete: true,
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.deleted, 2, "all 2 should be hard-deleted");
    assert_eq!(resp.soft_deleted, 0, "none should be soft-deleted");
}

// ══════════════════════════════════════════════════════════════════════════════
// Versioning RPCs (collection without versions)
// ══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn list_versions_no_versioning() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let doc = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(make_struct(&[("title", "V Test")])),
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
            collection: "posts".to_string(),
            id: doc.id,
            limit: None,
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    assert!(err.message().contains("versioning"));
}

#[tokio::test]
async fn restore_version_no_versioning() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let err = ts
        .service
        .restore_version(Request::new(content::RestoreVersionRequest {
            collection: "posts".to_string(),
            document_id: "some-id".to_string(),
            version_id: "some-version".to_string(),
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
}

// ══════════════════════════════════════════════════════════════════════════════
// Job RPCs (unauthenticated)
// ══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn list_jobs_unauthenticated() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let err = ts
        .service
        .list_jobs(Request::new(content::ListJobsRequest {}))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::Unauthenticated);
}

#[tokio::test]
async fn trigger_job_unauthenticated() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let err = ts
        .service
        .trigger_job(Request::new(content::TriggerJobRequest {
            slug: "cleanup".to_string(),
            data_json: None,
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::Unauthenticated);
}

#[tokio::test]
async fn get_job_run_unauthenticated() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let err = ts
        .service
        .get_job_run(Request::new(content::GetJobRunRequest {
            id: "some-id".to_string(),
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::Unauthenticated);
}

#[tokio::test]
async fn list_job_runs_unauthenticated() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let err = ts
        .service
        .list_job_runs(Request::new(content::ListJobRunsRequest {
            slug: None,
            status: None,
            limit: None,
            offset: None,
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::Unauthenticated);
}

// ══════════════════════════════════════════════════════════════════════════════
// FindByID with select fields
// ══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn find_by_id_with_select() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let doc = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(make_struct(&[
                ("title", "Select Me"),
                ("status", "published"),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    let found = ts
        .service
        .find_by_id(Request::new(content::FindByIdRequest {
            collection: "posts".to_string(),
            id: doc.id.clone(),
            depth: Some(0),
            locale: None,
            select: vec!["title".to_string()],
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    assert!(get_proto_field(&found, "title").is_some());
    // status should be stripped by select
    assert!(get_proto_field(&found, "status").is_none());
}

// ══════════════════════════════════════════════════════════════════════════════
// Update global nonexistent
// ══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn update_global_nonexistent() {
    let ts = setup_service(vec![], vec![]);

    let err = ts
        .service
        .update_global(Request::new(content::UpdateGlobalRequest {
            slug: "nonexistent".to_string(),
            data: Some(make_struct(&[("key", "value")])),
            locale: None,
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::NotFound);
}

// ══════════════════════════════════════════════════════════════════════════════
// Describe collection with auth and upload flags
// ══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn describe_auth_collection() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    let resp = ts
        .service
        .describe_collection(Request::new(content::DescribeCollectionRequest {
            slug: "users".to_string(),
            is_global: false,
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.slug, "users");
    assert!(resp.auth);
    assert!(resp.timestamps);
    assert!(!resp.upload);
    assert!(!resp.drafts);
}

// ══════════════════════════════════════════════════════════════════════════════
// FindByID not found
// ══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn find_by_id_not_found() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let err = ts
        .service
        .find_by_id(Request::new(content::FindByIdRequest {
            collection: "posts".to_string(),
            id: "nonexistent-id".to_string(),
            depth: Some(0),
            locale: None,
            select: vec![],
            draft: None,
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::NotFound);
}

// ══════════════════════════════════════════════════════════════════════════════
// Delete nonexistent collection
// ══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn delete_nonexistent_collection() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let err = ts
        .service
        .delete(Request::new(content::DeleteRequest {
            collection: "nonexistent".to_string(),
            id: "some-id".to_string(),
            force_hard_delete: false,
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::NotFound);
}

#[tokio::test]
async fn update_nonexistent_collection() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let err = ts
        .service
        .update(Request::new(content::UpdateRequest {
            collection: "nonexistent".to_string(),
            id: "some-id".to_string(),
            data: Some(make_struct(&[("title", "Test")])),
            locale: None,
            draft: None,
            unpublish: None,
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::NotFound);
}

// ── FTS Search Tests ──────────────────────────────────────────────────────

#[tokio::test]
async fn find_with_search() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    // Create posts with distinct titles
    for title in &[
        "Rust Programming Guide",
        "Python Tutorial",
        "Advanced Rust Patterns",
    ] {
        ts.service
            .create(Request::new(content::CreateRequest {
                collection: "posts".to_string(),
                data: Some(make_struct(&[("title", title)])),
                locale: None,
                draft: None,
            }))
            .await
            .unwrap();
    }

    // Search for "Rust"
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "posts".to_string(),
            search: Some("Rust".to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 2);
    assert_eq!(resp.documents.len(), 2);

    // All results should contain "Rust" in the title
    for doc in &resp.documents {
        let title = get_proto_field(doc, "title").unwrap();
        assert!(
            title.contains("Rust"),
            "Expected Rust in title, got: {}",
            title
        );
    }
}

#[tokio::test]
async fn find_with_search_no_results() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(make_struct(&[("title", "Hello World")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "posts".to_string(),
            search: Some("nonexistent_xyz".to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 0);
    assert!(resp.documents.is_empty());
}

#[tokio::test]
async fn find_with_search_and_where() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    // Create posts with different statuses
    for (title, status) in &[
        ("Rust Basics", "published"),
        ("Rust Advanced", "draft"),
        ("Python Basics", "published"),
    ] {
        ts.service
            .create(Request::new(content::CreateRequest {
                collection: "posts".to_string(),
                data: Some(make_struct(&[("title", title), ("status", status)])),
                locale: None,
                draft: None,
            }))
            .await
            .unwrap();
    }

    // Search for "Rust" + filter by status=published
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "posts".to_string(),
            search: Some("Rust".to_string()),
            r#where: Some(r#"{"status": "published"}"#.to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    // Only "Rust Basics" should match (Rust + published)
    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 1);
    assert_eq!(
        get_proto_field(&resp.documents[0], "title").as_deref(),
        Some("Rust Basics")
    );
}

#[tokio::test]
async fn count_with_search() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    for title in &["Rust Guide", "Rust Tutorial", "Python Guide"] {
        ts.service
            .create(Request::new(content::CreateRequest {
                collection: "posts".to_string(),
                data: Some(make_struct(&[("title", title)])),
                locale: None,
                draft: None,
            }))
            .await
            .unwrap();
    }

    let resp = ts
        .service
        .count(Request::new(content::CountRequest {
            collection: "posts".to_string(),
            search: Some("Rust".to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.count, 2);
}

#[tokio::test]
async fn count_with_search_and_where() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    for (title, status) in &[
        ("Rust A", "published"),
        ("Rust B", "draft"),
        ("Python A", "published"),
    ] {
        ts.service
            .create(Request::new(content::CreateRequest {
                collection: "posts".to_string(),
                data: Some(make_struct(&[("title", title), ("status", status)])),
                locale: None,
                draft: None,
            }))
            .await
            .unwrap();
    }

    let resp = ts
        .service
        .count(Request::new(content::CountRequest {
            collection: "posts".to_string(),
            search: Some("Rust".to_string()),
            r#where: Some(r#"{"status": "published"}"#.to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.count, 1);
}

#[tokio::test]
async fn find_with_search_empty_string_returns_all() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    for title in &["A", "B"] {
        ts.service
            .create(Request::new(content::CreateRequest {
                collection: "posts".to_string(),
                data: Some(make_struct(&[("title", title)])),
                locale: None,
                draft: None,
            }))
            .await
            .unwrap();
    }

    // Empty search string should return all documents
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "posts".to_string(),
            search: Some("".to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 2);
}
