//! Regression tests for reference counting bugs.
//!
//! Each test reproduces a specific bug that was found and fixed. These tests
//! must not be removed — they prevent silent reintroduction of data corruption.

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
use crap_cms::db::{migrate, pool, query};
use crap_cms::hooks::lifecycle::HookRunner;

// ── Helpers ──────────────────────────────────────────────────────────────

fn str_val(s: &str) -> Value {
    Value {
        kind: Some(Kind::StringValue(s.to_string())),
    }
}

fn make_struct(pairs: &[(&str, &str)]) -> Struct {
    let mut fields = BTreeMap::new();
    for (k, v) in pairs {
        fields.insert(k.to_string(), str_val(v));
    }
    Struct { fields }
}

struct TestSetup {
    _tmp: tempfile::TempDir,
    service: ContentService,
    pool: crap_cms::db::DbPool,
}

fn setup(collections: Vec<CollectionDefinition>) -> TestSetup {
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
    }

    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync schema");

    let hook_runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(registry.clone())
        .config(&config)
        .build()
        .expect("create hook runner");

    let email_renderer = Arc::new(EmailRenderer::new(tmp.path()).expect("email renderer"));

    let deps = ContentServiceDeps::builder()
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
        .login_limiter(Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
            5, 300,
        )))
        .ip_login_limiter(Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
            20, 300,
        )))
        .forgot_password_limiter(Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
            3, 900,
        )))
        .ip_forgot_password_limiter(Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
            20, 900,
        )));

    let service = ContentService::new(deps.build());

    TestSetup {
        _tmp: tmp,
        service,
        pool: db_pool,
    }
}

fn make_posts_and_tags() -> Vec<CollectionDefinition> {
    let mut tags = CollectionDefinition::new("tags");
    tags.admin.use_as_title = Some("name".to_string());
    tags.fields = vec![FieldDefinition {
        name: "name".to_string(),
        field_type: FieldType::Text,
        ..Default::default()
    }];

    let mut posts = CollectionDefinition::new("posts");
    posts.admin.use_as_title = Some("title".to_string());
    posts.fields = vec![
        FieldDefinition {
            name: "title".to_string(),
            field_type: FieldType::Text,
            ..Default::default()
        },
        FieldDefinition {
            name: "tag".to_string(),
            field_type: FieldType::Relationship,
            relationship: Some(RelationshipConfig::new("tags", false)),
            ..Default::default()
        },
    ];

    vec![tags, posts]
}

fn get_ref_count(setup: &TestSetup, collection: &str, id: &str) -> i64 {
    let conn = setup.pool.get().unwrap();
    query::ref_count::get_ref_count(&conn, collection, id)
        .unwrap()
        .expect("document should exist")
}

// ── Regression: UpdateMany must adjust ref counts ────────────────────────

#[tokio::test]
async fn update_many_adjusts_ref_counts() {
    let setup = setup(make_posts_and_tags());

    // Create two tags
    let tag_a_id = setup
        .service
        .create(Request::new(content::CreateRequest {
            collection: "tags".into(),
            data: Some(make_struct(&[("name", "Tag A")])),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap()
        .id;

    let tag_b_id = setup
        .service
        .create(Request::new(content::CreateRequest {
            collection: "tags".into(),
            data: Some(make_struct(&[("name", "Tag B")])),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap()
        .id;

    // Create a post referencing tag A
    setup
        .service
        .create(Request::new(content::CreateRequest {
            collection: "posts".into(),
            data: Some(make_struct(&[("title", "Post 1"), ("tag", &tag_a_id)])),
            ..Default::default()
        }))
        .await
        .unwrap();

    assert_eq!(get_ref_count(&setup, "tags", &tag_a_id), 1);
    assert_eq!(get_ref_count(&setup, "tags", &tag_b_id), 0);

    // UpdateMany: change all posts to reference tag B
    setup
        .service
        .update_many(Request::new(content::UpdateManyRequest {
            collection: "posts".into(),
            data: Some(make_struct(&[("tag", &tag_b_id)])),
            ..Default::default()
        }))
        .await
        .unwrap();

    assert_eq!(
        get_ref_count(&setup, "tags", &tag_a_id),
        0,
        "Tag A ref_count should be 0 after UpdateMany changed reference"
    );
    assert_eq!(
        get_ref_count(&setup, "tags", &tag_b_id),
        1,
        "Tag B ref_count should be 1 after UpdateMany"
    );
}

// ── Regression: DeleteMany must adjust ref counts ────────────────────────

#[tokio::test]
async fn delete_many_adjusts_ref_counts() {
    let setup = setup(make_posts_and_tags());

    let tag_id = setup
        .service
        .create(Request::new(content::CreateRequest {
            collection: "tags".into(),
            data: Some(make_struct(&[("name", "Tag")])),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap()
        .id;

    // Create two posts referencing the tag
    for i in 1..=2 {
        setup
            .service
            .create(Request::new(content::CreateRequest {
                collection: "posts".into(),
                data: Some(make_struct(&[
                    ("title", &format!("Post {i}")),
                    ("tag", &tag_id),
                ])),
                ..Default::default()
            }))
            .await
            .unwrap();
    }

    assert_eq!(get_ref_count(&setup, "tags", &tag_id), 2);

    // DeleteMany all posts
    setup
        .service
        .delete_many(Request::new(content::DeleteManyRequest {
            collection: "posts".into(),
            ..Default::default()
        }))
        .await
        .unwrap();

    assert_eq!(
        get_ref_count(&setup, "tags", &tag_id),
        0,
        "Tag ref_count should be 0 after DeleteMany removed all referencing posts"
    );
}

// ── Regression: DeleteMany skips protected documents ─────────────────────

#[tokio::test]
async fn delete_many_skips_referenced_documents() {
    let setup = setup(make_posts_and_tags());

    let tag_id = setup
        .service
        .create(Request::new(content::CreateRequest {
            collection: "tags".into(),
            data: Some(make_struct(&[("name", "Protected Tag")])),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap()
        .id;

    // Create a post referencing it
    setup
        .service
        .create(Request::new(content::CreateRequest {
            collection: "posts".into(),
            data: Some(make_struct(&[("title", "Post"), ("tag", &tag_id)])),
            ..Default::default()
        }))
        .await
        .unwrap();

    assert_eq!(get_ref_count(&setup, "tags", &tag_id), 1);

    // Try to DeleteMany all tags — referenced tag should be skipped
    let resp = setup
        .service
        .delete_many(Request::new(content::DeleteManyRequest {
            collection: "tags".into(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.deleted, 0, "Should skip tags with ref_count > 0");

    // Tag should still exist
    let found = setup
        .service
        .find_by_id(Request::new(content::FindByIdRequest {
            collection: "tags".into(),
            id: tag_id,
            ..Default::default()
        }))
        .await;
    assert!(found.is_ok(), "Protected tag should still exist");
}

// ── Regression: Version restore must adjust ref counts ───────────────────

#[tokio::test]
async fn version_restore_adjusts_ref_counts() {
    let mut collections = make_posts_and_tags();
    collections[1].versions = Some(VersionsConfig::new(true, 0));
    let setup = setup(collections);

    let tag_a_id = setup
        .service
        .create(Request::new(content::CreateRequest {
            collection: "tags".into(),
            data: Some(make_struct(&[("name", "A")])),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap()
        .id;

    let tag_b_id = setup
        .service
        .create(Request::new(content::CreateRequest {
            collection: "tags".into(),
            data: Some(make_struct(&[("name", "B")])),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap()
        .id;

    // Create post referencing tag A (version 1)
    let post_id = setup
        .service
        .create(Request::new(content::CreateRequest {
            collection: "posts".into(),
            data: Some(make_struct(&[("title", "Post"), ("tag", &tag_a_id)])),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap()
        .id;

    assert_eq!(get_ref_count(&setup, "tags", &tag_a_id), 1);
    assert_eq!(get_ref_count(&setup, "tags", &tag_b_id), 0);

    // Update post to reference tag B (version 2)
    setup
        .service
        .update(Request::new(content::UpdateRequest {
            collection: "posts".into(),
            id: post_id.clone(),
            data: Some(make_struct(&[("tag", &tag_b_id)])),
            ..Default::default()
        }))
        .await
        .unwrap();

    assert_eq!(get_ref_count(&setup, "tags", &tag_a_id), 0);
    assert_eq!(get_ref_count(&setup, "tags", &tag_b_id), 1);

    // Get version 1 (oldest — last in newest-first list)
    let versions = setup
        .service
        .list_versions(Request::new(content::ListVersionsRequest {
            collection: "posts".into(),
            id: post_id.clone(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    let v1_id = &versions.versions.last().unwrap().id;

    // Restore version 1 (which references tag A)
    setup
        .service
        .restore_version(Request::new(content::RestoreVersionRequest {
            collection: "posts".into(),
            document_id: post_id,
            version_id: v1_id.clone(),
        }))
        .await
        .unwrap();

    assert_eq!(
        get_ref_count(&setup, "tags", &tag_a_id),
        1,
        "Tag A ref_count should be 1 after restoring version that references it"
    );
    assert_eq!(
        get_ref_count(&setup, "tags", &tag_b_id),
        0,
        "Tag B ref_count should be 0 after restoring version that doesn't reference it"
    );
}
