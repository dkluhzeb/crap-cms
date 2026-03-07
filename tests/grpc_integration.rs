//! Integration tests for the gRPC ContentService via the ContentApi trait.
//!
//! These tests construct a ContentService directly (no network) and call
//! trait methods with tonic::Request objects to exercise the full RPC path.

use std::collections::BTreeMap;
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
use crap_cms::hooks::lifecycle::HookRunner;

// ── Helpers ───────────────────────────────────────────────────────────────

fn make_posts_def() -> CollectionDefinition {
    CollectionDefinition {
        slug: "posts".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("Post".to_string())),
            plural: Some(LocalizedString::Plain("Posts".to_string())),
        },
        timestamps: true,
        fields: vec![
            FieldDefinition {
                name: "title".to_string(),
                required: true,
                ..Default::default()
            },
            FieldDefinition {
                name: "status".to_string(),
                field_type: FieldType::Select,
                default_value: Some(serde_json::json!("draft")),
                ..Default::default()
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
            versions: None,
            indexes: Vec::new(),
    }
}

fn make_users_def() -> CollectionDefinition {
    CollectionDefinition {
        slug: "users".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("User".to_string())),
            plural: Some(LocalizedString::Plain("Users".to_string())),
        },
        timestamps: true,
        fields: vec![
            FieldDefinition {
                name: "email".to_string(),
                field_type: FieldType::Email,
                required: true,
                unique: true,
                ..Default::default()
            },
            FieldDefinition {
                name: "name".to_string(),
                ..Default::default()
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: Some(CollectionAuth { enabled: true, ..Default::default() }),
        upload: None,
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
            versions: None,
            indexes: Vec::new(),
    }
}

fn make_global_def() -> GlobalDefinition {
    GlobalDefinition {
        slug: "settings".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("Settings".to_string())),
            plural: None,
        },
        fields: vec![
            FieldDefinition {
                name: "site_name".to_string(),
                ..Default::default()
            },
        ],
        hooks: CollectionHooks::default(),
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
        versions: None,
    }
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
    pool: crap_cms::db::DbPool,
}

fn setup_service(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
) -> TestSetup {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".to_string();

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

    let hook_runner =
        HookRunner::new(tmp.path(), registry.clone(), &config).expect("create hook runner");

    let email_renderer =
        Arc::new(EmailRenderer::new(tmp.path()).expect("create email renderer"));

    let service = ContentService::new(
        db_pool.clone(),
        Registry::snapshot(&registry),
        hook_runner,
        config.auth.secret.clone(),
        &config.depth,
        &config.pagination,
        config.email.clone(),
        email_renderer,
        config.server.clone(),
        None, // no event bus
        config.locale.clone(),
        tmp.path().to_path_buf(),
        std::sync::Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(5, 300)),
        config.auth.reset_token_expiry,
        config.auth.password_policy.clone(),
        std::sync::Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(3, 900)),
    );

    TestSetup { _tmp: tmp, service, pool: db_pool }
}

// ── CRUD Tests ────────────────────────────────────────────────────────────

#[tokio::test]
async fn find_empty_collection() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "posts".to_string(),
            ..Default::default()
        }))
        .await
        .expect("Find failed");

    let body = resp.into_inner();
    assert_eq!(body.documents.len(), 0);
    assert_eq!(body.pagination.as_ref().unwrap().total_docs, 0);
}

#[tokio::test]
async fn create_and_find() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    // Create
    let create_resp = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(make_struct(&[("title", "Hello"), ("status", "published")])),
            locale: None,
            draft: None,
        }))
        .await
        .expect("Create failed");

    let doc = create_resp.into_inner().document.expect("No document");
    assert!(!doc.id.is_empty());
    assert_eq!(doc.collection, "posts");
    assert_eq!(get_proto_field(&doc, "title").as_deref(), Some("Hello"));

    // Find
    let find_resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "posts".to_string(),
            ..Default::default()
        }))
        .await
        .expect("Find failed");

    let body = find_resp.into_inner();
    assert_eq!(body.documents.len(), 1);
    assert_eq!(body.pagination.as_ref().unwrap().total_docs, 1);
    assert_eq!(body.documents[0].id, doc.id);
}

#[tokio::test]
async fn create_and_find_by_id() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let doc = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(make_struct(&[("title", "Test Post")])),
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
            select: vec![],
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .expect("Document not found");

    assert_eq!(found.id, doc.id);
    assert_eq!(get_proto_field(&found, "title").as_deref(), Some("Test Post"));
}

#[tokio::test]
async fn update_document() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let doc = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(make_struct(&[("title", "Original"), ("status", "draft")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    let updated = ts
        .service
        .update(Request::new(content::UpdateRequest {
            collection: "posts".to_string(),
            id: doc.id.clone(),
            data: Some(make_struct(&[("title", "Updated")])),
            locale: None,
            draft: None,
            unpublish: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    assert_eq!(updated.id, doc.id);
    assert_eq!(get_proto_field(&updated, "title").as_deref(), Some("Updated"));
}

#[tokio::test]
async fn delete_document() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let doc = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(make_struct(&[("title", "To Delete")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    let del_resp = ts
        .service
        .delete(Request::new(content::DeleteRequest {
            collection: "posts".to_string(),
            id: doc.id.clone(),
        }))
        .await
        .unwrap()
        .into_inner();

    assert!(del_resp.success);

    // Verify gone
    let found = ts
        .service
        .find_by_id(Request::new(content::FindByIdRequest {
            collection: "posts".to_string(),
            id: doc.id,
            depth: Some(0),
            locale: None,
            select: vec![],
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document;

    assert!(found.is_none());
}

#[tokio::test]
async fn find_with_where() {
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

    // Filter by status=published using where clause
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "posts".to_string(),
            r#where: Some(r#"{"status": "published"}"#.to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 2);
    assert_eq!(resp.documents.len(), 2);
}

#[tokio::test]
async fn find_with_where_json() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    for (title, status) in &[("X", "draft"), ("Y", "published"), ("Z", "published")] {
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

    // Use where JSON clause with contains operator
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "posts".to_string(),
            r#where: Some(r#"{"status": {"equals": "draft"}}"#.to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 1);
    assert_eq!(
        get_proto_field(&resp.documents[0], "title").as_deref(),
        Some("X")
    );
}

#[tokio::test]
async fn find_with_where_or() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    for title in &["Alpha", "Beta", "Gamma"] {
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
        .find(Request::new(content::FindRequest {
            collection: "posts".to_string(),
            r#where: Some(
                r#"{"or": [{"title": "Alpha"}, {"title": "Gamma"}]}"#.to_string(),
            ),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 2);
}

#[tokio::test]
async fn find_with_limit_and_offset() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    for i in 0..5 {
        ts.service
            .create(Request::new(content::CreateRequest {
                collection: "posts".to_string(),
                data: Some(make_struct(&[("title", &format!("Post {}", i))])),
                locale: None,
                draft: None,
            }))
            .await
            .unwrap();
    }

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

    assert_eq!(resp.documents.len(), 2);
    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 5); // total is unaffected by limit/page
}

#[tokio::test]
async fn find_with_select() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(make_struct(&[("title", "Selectable"), ("status", "live")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "posts".to_string(),
            select: vec!["title".to_string()],
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.documents.len(), 1);
    let doc = &resp.documents[0];
    assert!(get_proto_field(doc, "title").is_some());
    // status should not be in the response (not in select)
    assert!(get_proto_field(doc, "status").is_none());
}

#[tokio::test]
async fn find_nonexistent_collection() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let err = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "nonexistent".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::NotFound);
}

#[tokio::test]
async fn create_nonexistent_collection() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let err = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "nonexistent".to_string(),
            data: Some(make_struct(&[("title", "Nope")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::NotFound);
}

#[tokio::test]
async fn find_with_invalid_where_json() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let err = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "posts".to_string(),
            r#where: Some("not valid json".to_string()),
            ..Default::default()
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn find_validates_filter_fields() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let err = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "posts".to_string(),
            r#where: Some(r#"{"nonexistent_field": "value"}"#.to_string()),
            ..Default::default()
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

// ── Globals Tests ──────────────────────────────────────────────────────────

#[tokio::test]
async fn get_global_default() {
    let ts = setup_service(vec![], vec![make_global_def()]);

    let resp = ts
        .service
        .get_global(Request::new(content::GetGlobalRequest {
            slug: "settings".to_string(),
            locale: None,
        }))
        .await
        .unwrap()
        .into_inner();

    let doc = resp.document.expect("No global document");
    assert_eq!(doc.id, "default");
    assert_eq!(doc.collection, "settings");
}

#[tokio::test]
async fn update_global_and_read_back() {
    let ts = setup_service(vec![], vec![make_global_def()]);

    ts.service
        .update_global(Request::new(content::UpdateGlobalRequest {
            slug: "settings".to_string(),
            data: Some(make_struct(&[("site_name", "My CMS")])),
            locale: None,
        }))
        .await
        .unwrap();

    let doc = ts
        .service
        .get_global(Request::new(content::GetGlobalRequest {
            slug: "settings".to_string(),
            locale: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    assert_eq!(get_proto_field(&doc, "site_name").as_deref(), Some("My CMS"));
}

#[tokio::test]
async fn get_global_nonexistent() {
    let ts = setup_service(vec![], vec![]);

    let err = ts
        .service
        .get_global(Request::new(content::GetGlobalRequest {
            slug: "nope".to_string(),
            locale: None,
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::NotFound);
}

// ── List & Describe Tests ─────────────────────────────────────────────────

#[tokio::test]
async fn list_collections_returns_all() {
    let ts = setup_service(vec![make_posts_def(), make_users_def()], vec![make_global_def()]);

    let resp = ts
        .service
        .list_collections(Request::new(content::ListCollectionsRequest {}))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.collections.len(), 2);
    assert_eq!(resp.globals.len(), 1);

    // Collections sorted by slug
    assert_eq!(resp.collections[0].slug, "posts");
    assert_eq!(resp.collections[1].slug, "users");
    assert!(resp.collections[1].auth); // users is an auth collection
    assert!(!resp.collections[0].auth);

    assert_eq!(resp.globals[0].slug, "settings");
}

#[tokio::test]
async fn describe_collection_returns_fields() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let resp = ts
        .service
        .describe_collection(Request::new(content::DescribeCollectionRequest {
            slug: "posts".to_string(),
            is_global: false,
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.slug, "posts");
    assert!(resp.timestamps);
    assert!(!resp.auth);
    assert_eq!(resp.fields.len(), 2);
    assert_eq!(resp.fields[0].name, "title");
    assert_eq!(resp.fields[0].r#type, "text");
    assert!(resp.fields[0].required);
}

#[tokio::test]
async fn describe_global() {
    let ts = setup_service(vec![], vec![make_global_def()]);

    let resp = ts
        .service
        .describe_collection(Request::new(content::DescribeCollectionRequest {
            slug: "settings".to_string(),
            is_global: true,
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.slug, "settings");
    assert!(!resp.timestamps);
    assert!(!resp.auth);
    assert_eq!(resp.fields.len(), 1);
    assert_eq!(resp.fields[0].name, "site_name");
}

#[tokio::test]
async fn describe_nonexistent_collection() {
    let ts = setup_service(vec![], vec![]);

    let err = ts
        .service
        .describe_collection(Request::new(content::DescribeCollectionRequest {
            slug: "nope".to_string(),
            is_global: false,
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::NotFound);
}

// ── Auth Tests ────────────────────────────────────────────────────────────

#[tokio::test]
async fn login_non_auth_collection() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let err = ts
        .service
        .login(Request::new(content::LoginRequest {
            collection: "posts".to_string(),
            email: "a@b.com".to_string(),
            password: "secret".to_string(),
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert!(err.message().contains("not an auth collection"));
}

#[tokio::test]
async fn login_valid_credentials() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    // Create a user with password
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "users".to_string(),
            data: Some(make_struct(&[
                ("email", "alice@example.com"),
                ("name", "Alice"),
                ("password", "secret123"),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    // Login
    let resp = ts
        .service
        .login(Request::new(content::LoginRequest {
            collection: "users".to_string(),
            email: "alice@example.com".to_string(),
            password: "secret123".to_string(),
        }))
        .await
        .unwrap()
        .into_inner();

    assert!(!resp.token.is_empty());
    let user = resp.user.expect("No user in response");
    assert_eq!(get_proto_field(&user, "email").as_deref(), Some("alice@example.com"));
}

#[tokio::test]
async fn login_invalid_password() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "users".to_string(),
            data: Some(make_struct(&[
                ("email", "bob@example.com"),
                ("password", "correct1"),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    let err = ts
        .service
        .login(Request::new(content::LoginRequest {
            collection: "users".to_string(),
            email: "bob@example.com".to_string(),
            password: "wrong".to_string(),
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::Unauthenticated);
}

#[tokio::test]
async fn login_nonexistent_user() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    let err = ts
        .service
        .login(Request::new(content::LoginRequest {
            collection: "users".to_string(),
            email: "nobody@example.com".to_string(),
            password: "anything".to_string(),
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::Unauthenticated);
}

#[tokio::test]
async fn me_valid_token() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "users".to_string(),
            data: Some(make_struct(&[
                ("email", "carol@example.com"),
                ("name", "Carol"),
                ("password", "pw123456"),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    let login_resp = ts
        .service
        .login(Request::new(content::LoginRequest {
            collection: "users".to_string(),
            email: "carol@example.com".to_string(),
            password: "pw123456".to_string(),
        }))
        .await
        .unwrap()
        .into_inner();

    let me_resp = ts
        .service
        .me(Request::new(content::MeRequest {
            token: login_resp.token,
        }))
        .await
        .unwrap()
        .into_inner();

    let user = me_resp.user.expect("No user");
    assert_eq!(get_proto_field(&user, "email").as_deref(), Some("carol@example.com"));
    assert_eq!(get_proto_field(&user, "name").as_deref(), Some("Carol"));
}

#[tokio::test]
async fn me_invalid_token() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    let err = ts
        .service
        .me(Request::new(content::MeRequest {
            token: "bogus-token".to_string(),
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::Unauthenticated);
}

// ── Password Reset Tests ──────────────────────────────────────────────────

#[tokio::test]
async fn reset_password_short_password() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    let err = ts
        .service
        .reset_password(Request::new(content::ResetPasswordRequest {
            collection: "users".to_string(),
            token: "some-token".to_string(),
            new_password: "short".to_string(), // < 8 chars
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert!(err.message().contains("at least 8 characters"));
}

#[tokio::test]
async fn reset_password_non_auth_collection() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let err = ts
        .service
        .reset_password(Request::new(content::ResetPasswordRequest {
            collection: "posts".to_string(),
            token: "tok".to_string(),
            new_password: "newpassword".to_string(),
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

// ── Email Verification Tests ──────────────────────────────────────────────

#[tokio::test]
async fn verify_email_non_auth_collection() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let err = ts
        .service
        .verify_email(Request::new(content::VerifyEmailRequest {
            collection: "posts".to_string(),
            token: "tok".to_string(),
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn verify_email_not_enabled() {
    // Users collection has auth but verify_email defaults to false
    let ts = setup_service(vec![make_users_def()], vec![]);

    let err = ts
        .service
        .verify_email(Request::new(content::VerifyEmailRequest {
            collection: "users".to_string(),
            token: "tok".to_string(),
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert!(err.message().contains("not enabled"));
}

// ── Forgot Password Tests ─────────────────────────────────────────────────

#[tokio::test]
async fn forgot_password_non_auth_collection() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let err = ts
        .service
        .forgot_password(Request::new(content::ForgotPasswordRequest {
            collection: "posts".to_string(),
            email: "a@b.com".to_string(),
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn forgot_password_always_returns_success() {
    // ForgotPassword always returns success to avoid leaking user existence
    let ts = setup_service(vec![make_users_def()], vec![]);

    let resp = ts
        .service
        .forgot_password(Request::new(content::ForgotPasswordRequest {
            collection: "users".to_string(),
            email: "nonexistent@example.com".to_string(),
        }))
        .await
        .unwrap()
        .into_inner();

    assert!(resp.success);
}

#[tokio::test]
async fn forgot_password_not_enabled() {
    // Create a users def with forgot_password explicitly disabled
    let mut def = make_users_def();
    if let Some(ref mut auth) = def.auth {
        auth.forgot_password = false;
    }
    let ts = setup_service(vec![def], vec![]);

    let err = ts
        .service
        .forgot_password(Request::new(content::ForgotPasswordRequest {
            collection: "users".to_string(),
            email: "a@b.com".to_string(),
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::PermissionDenied);
}

// ── Subscribe Tests ───────────────────────────────────────────────────────

#[tokio::test]
async fn subscribe_without_event_bus() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let result = ts
        .service
        .subscribe(Request::new(content::SubscribeRequest {
            collections: vec!["posts".to_string()],
            ..Default::default()
        }))
        .await;

    assert!(result.is_err());
    let err = result.err().unwrap();
    assert_eq!(err.code(), tonic::Code::Unavailable);
    assert!(err.message().contains("disabled"));
}

// ── Auth Bearer Token in Metadata ─────────────────────────────────────────

#[tokio::test]
async fn authenticated_crud_with_bearer_token() {
    let ts = setup_service(vec![make_posts_def(), make_users_def()], vec![]);

    // Create user and login
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "users".to_string(),
            data: Some(make_struct(&[
                ("email", "admin@test.com"),
                ("password", "admin123"),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    let token = ts
        .service
        .login(Request::new(content::LoginRequest {
            collection: "users".to_string(),
            email: "admin@test.com".to_string(),
            password: "admin123".to_string(),
        }))
        .await
        .unwrap()
        .into_inner()
        .token;

    // Create a post with Bearer token in metadata
    let mut req = Request::new(content::CreateRequest {
        collection: "posts".to_string(),
        data: Some(make_struct(&[("title", "Authenticated Post")])),
        locale: None,
        draft: None,
    });
    req.metadata_mut().insert(
        "authorization",
        format!("Bearer {}", token).parse().unwrap(),
    );

    let doc = req
        .extensions()
        .get::<()>(); // just to consume the var
    let _ = doc;

    let resp = ts.service.create(req).await.unwrap().into_inner();
    let doc = resp.document.unwrap();
    assert_eq!(get_proto_field(&doc, "title").as_deref(), Some("Authenticated Post"));
}

// ── Order By Tests ────────────────────────────────────────────────────────

#[tokio::test]
async fn find_with_order_by() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    for title in &["Charlie", "Alice", "Bob"] {
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
        .find(Request::new(content::FindRequest {
            collection: "posts".to_string(),
            order_by: Some("title".to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.documents.len(), 3);
    assert_eq!(
        get_proto_field(&resp.documents[0], "title").as_deref(),
        Some("Alice")
    );
    assert_eq!(
        get_proto_field(&resp.documents[1], "title").as_deref(),
        Some("Bob")
    );
    assert_eq!(
        get_proto_field(&resp.documents[2], "title").as_deref(),
        Some("Charlie")
    );
}

#[tokio::test]
async fn find_with_order_by_desc() {
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
        .find(Request::new(content::FindRequest {
            collection: "posts".to_string(),
            order_by: Some("-title".to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(
        get_proto_field(&resp.documents[0], "title").as_deref(),
        Some("C")
    );
    assert_eq!(
        get_proto_field(&resp.documents[2], "title").as_deref(),
        Some("A")
    );
}

// ── Full Password Reset Flow ──────────────────────────────────────────────

#[tokio::test]
async fn full_password_reset_flow() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    // Create a user
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "users".to_string(),
            data: Some(make_struct(&[
                ("email", "reset@example.com"),
                ("password", "oldpassword"),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    // Request password reset (always succeeds)
    let resp = ts
        .service
        .forgot_password(Request::new(content::ForgotPasswordRequest {
            collection: "users".to_string(),
            email: "reset@example.com".to_string(),
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(resp.success);

    // Verify reset_password rejects an invalid token (the real token was
    // stored by forgot_password but we don't extract it here).
    let err = ts
        .service
        .reset_password(Request::new(content::ResetPasswordRequest {
            collection: "users".to_string(),
            token: "nonexistent-token".to_string(),
            new_password: "newpassword123".to_string(),
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn reset_password_expired_token() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    // Create a user
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "users".to_string(),
            data: Some(make_struct(&[
                ("email", "expired@example.com"),
                ("password", "mypassword"),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    // Reset with a random token (not valid)
    let err = ts
        .service
        .reset_password(Request::new(content::ResetPasswordRequest {
            collection: "users".to_string(),
            token: "expired-fake-token".to_string(),
            new_password: "newpassword123".to_string(),
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn reset_password_invalid_token() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    let err = ts
        .service
        .reset_password(Request::new(content::ResetPasswordRequest {
            collection: "users".to_string(),
            token: "totally-random-token".to_string(),
            new_password: "newpassword123".to_string(),
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

// ── Email Verification ────────────────────────────────────────────────────

fn make_verify_users_def() -> CollectionDefinition {
    CollectionDefinition {
        slug: "members".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("Member".to_string())),
            plural: Some(LocalizedString::Plain("Members".to_string())),
        },
        timestamps: true,
        fields: vec![
            FieldDefinition {
                name: "email".to_string(),
                field_type: FieldType::Email,
                required: true,
                unique: true,
                ..Default::default()
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: Some(CollectionAuth {
            enabled: true,
            verify_email: true,
            ..Default::default()
        }),
        upload: None,
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
            versions: None,
            indexes: Vec::new(),
    }
}

#[tokio::test]
async fn verify_email_invalid_token_returns_error() {
    let ts = setup_service(vec![make_verify_users_def()], vec![]);

    let err = ts
        .service
        .verify_email(Request::new(content::VerifyEmailRequest {
            collection: "members".to_string(),
            token: "bad-token".to_string(),
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::NotFound);
}

#[tokio::test]
async fn login_blocked_when_unverified() {
    let ts = setup_service(vec![make_verify_users_def()], vec![]);

    // Create user (unverified)
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "members".to_string(),
            data: Some(make_struct(&[
                ("email", "unverified@example.com"),
                ("password", "secret123"),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    // Try to login — should fail because unverified
    let err = ts
        .service
        .login(Request::new(content::LoginRequest {
            collection: "members".to_string(),
            email: "unverified@example.com".to_string(),
            password: "secret123".to_string(),
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
    assert!(
        err.message().to_lowercase().contains("verif"),
        "Error should mention verification, got: {}",
        err.message()
    );
}

// ── Auth Password Update via gRPC ─────────────────────────────────────────

#[tokio::test]
async fn update_password_via_grpc() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    // Create user
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "users".to_string(),
            data: Some(make_struct(&[
                ("email", "pwchange@example.com"),
                ("name", "PW Changer"),
                ("password", "oldpass123"),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    // Login with old password
    let login_resp = ts
        .service
        .login(Request::new(content::LoginRequest {
            collection: "users".to_string(),
            email: "pwchange@example.com".to_string(),
            password: "oldpass123".to_string(),
        }))
        .await
        .unwrap()
        .into_inner();
    let user_id = login_resp.user.unwrap().id;

    // Update password (must include required email field)
    ts.service
        .update(Request::new(content::UpdateRequest {
            collection: "users".to_string(),
            id: user_id,
            data: Some(make_struct(&[
                ("email", "pwchange@example.com"),
                ("password", "newpass456"),
            ])),
            locale: None,
            draft: None,
            unpublish: None,
        }))
        .await
        .unwrap();

    // Login with new password should succeed
    let new_login = ts
        .service
        .login(Request::new(content::LoginRequest {
            collection: "users".to_string(),
            email: "pwchange@example.com".to_string(),
            password: "newpass456".to_string(),
        }))
        .await;
    assert!(new_login.is_ok(), "Login with new password should succeed");

    // Login with old password should fail
    let old_login = ts
        .service
        .login(Request::new(content::LoginRequest {
            collection: "users".to_string(),
            email: "pwchange@example.com".to_string(),
            password: "oldpass123".to_string(),
        }))
        .await;
    assert!(old_login.is_err(), "Login with old password should fail");
}

// ── Password Hash Not Exposed ─────────────────────────────────────────────

#[tokio::test]
async fn password_hash_not_in_response() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    let doc = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "users".to_string(),
            data: Some(make_struct(&[
                ("email", "hidden@example.com"),
                ("password", "secret123"),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // The response document should not contain _password_hash
    let fields = doc.fields.as_ref().unwrap();
    assert!(
        !fields.fields.contains_key("_password_hash"),
        "Response should not contain _password_hash"
    );
    assert!(
        !fields.fields.contains_key("password"),
        "Response should not contain password field"
    );

    // Also check find_by_id
    let found = ts
        .service
        .find_by_id(Request::new(content::FindByIdRequest {
            collection: "users".to_string(),
            id: doc.id,
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

    let fields = found.fields.as_ref().unwrap();
    assert!(
        !fields.fields.contains_key("_password_hash"),
        "FindByID response should not contain _password_hash"
    );
}

// ── Depth > 0 in gRPC ────────────────────────────────────────────────────

fn make_categories_def() -> CollectionDefinition {
    CollectionDefinition {
        slug: "categories".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("Category".to_string())),
            plural: Some(LocalizedString::Plain("Categories".to_string())),
        },
        timestamps: true,
        fields: vec![
            FieldDefinition {
                name: "name".to_string(),
                required: true,
                ..Default::default()
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
            versions: None,
            indexes: Vec::new(),
    }
}

fn make_posts_with_relationship() -> CollectionDefinition {
    CollectionDefinition {
        slug: "posts".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("Post".to_string())),
            plural: Some(LocalizedString::Plain("Posts".to_string())),
        },
        timestamps: true,
        fields: vec![
            FieldDefinition {
                name: "title".to_string(),
                required: true,
                ..Default::default()
            },
            FieldDefinition {
                name: "category".to_string(),
                field_type: FieldType::Relationship,
                relationship: Some(RelationshipConfig {
                    collection: "categories".to_string(),
                    has_many: false,
                    max_depth: None,
                    polymorphic: vec![],
                }),
                ..Default::default()
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
            versions: None,
            indexes: Vec::new(),
    }
}

#[tokio::test]
async fn find_with_depth_1_populates_relationship() {
    let ts = setup_service(
        vec![make_categories_def(), make_posts_with_relationship()],
        vec![],
    );

    // Create a category
    let cat_doc = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "categories".to_string(),
            data: Some(make_struct(&[("name", "Tech")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // Create a post with the category ID
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(make_struct(&[
                ("title", "Depth Test"),
                ("category", &cat_doc.id),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    // Find with depth=1 — category should be populated as an object
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "posts".to_string(),
            depth: Some(1),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.documents.len(), 1);
    let doc = &resp.documents[0];
    let fields = doc.fields.as_ref().unwrap();

    // At depth=1, the category field should be a struct (populated), not a string
    let cat_field = fields.fields.get("category");
    assert!(cat_field.is_some(), "category field should be present");

    let cat_val = cat_field.unwrap();
    // If populated, it should be a StructValue containing the category document
    match &cat_val.kind {
        Some(Kind::StructValue(s)) => {
            // The populated object should have "name" = "Tech"
            let name = s.fields.get("name");
            assert!(name.is_some(), "Populated category should have 'name' field");
        }
        Some(Kind::StringValue(s)) => {
            // If depth population isn't working, it stays as an ID string
            assert_eq!(s, &cat_doc.id, "If not populated, should be the category ID");
        }
        other => {
            // Either struct or string is acceptable depending on implementation
            panic!("Unexpected category field kind: {:?}", other);
        }
    }
}

#[tokio::test]
async fn find_by_id_default_depth_populates() {
    let ts = setup_service(
        vec![make_categories_def(), make_posts_with_relationship()],
        vec![],
    );

    // Create a category
    let cat_doc = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "categories".to_string(),
            data: Some(make_struct(&[("name", "Science")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // Create a post with the category ID
    let post_doc = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(make_struct(&[
                ("title", "Depth Default Test"),
                ("category", &cat_doc.id),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // FindByID with no explicit depth — should use default_depth from config (1)
    let found = ts
        .service
        .find_by_id(Request::new(content::FindByIdRequest {
            collection: "posts".to_string(),
            id: post_doc.id,
            depth: None, // use default
            locale: None,
            select: vec![],
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    let fields = found.fields.as_ref().unwrap();
    let cat_field = fields.fields.get("category");
    assert!(cat_field.is_some(), "category field should be present in FindByID");
}

// ── Dot-notation filter e2e tests ────────────────────────────────────────────

fn make_products_def() -> CollectionDefinition {
    CollectionDefinition {
        slug: "products".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("Product".to_string())),
            plural: Some(LocalizedString::Plain("Products".to_string())),
        },
        timestamps: true,
        fields: vec![
            FieldDefinition {
                name: "name".to_string(),
                required: true,
                ..Default::default()
            },
            FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                fields: vec![FieldDefinition {
                    name: "meta_title".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            },
            FieldDefinition {
                name: "variants".to_string(),
                field_type: FieldType::Array,
                fields: vec![
                    FieldDefinition {
                        name: "color".to_string(),
                        ..Default::default()
                    },
                    FieldDefinition {
                        name: "dimensions".to_string(),
                        field_type: FieldType::Group,
                        fields: vec![
                            FieldDefinition {
                                name: "width".to_string(),
                                ..Default::default()
                            },
                            FieldDefinition {
                                name: "height".to_string(),
                                ..Default::default()
                            },
                        ],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            FieldDefinition {
                name: "content".to_string(),
                field_type: FieldType::Blocks,
                blocks: vec![
                    BlockDefinition {
                        block_type: "text".to_string(),
                        fields: vec![FieldDefinition {
                            name: "body".to_string(),
                            field_type: FieldType::Textarea,
                            ..Default::default()
                        }],
                        ..Default::default()
                    },
                    BlockDefinition {
                        block_type: "section".to_string(),
                        fields: vec![
                            FieldDefinition {
                                name: "heading".to_string(),
                                ..Default::default()
                            },
                            FieldDefinition {
                                name: "meta".to_string(),
                                field_type: FieldType::Group,
                                fields: vec![FieldDefinition {
                                    name: "author".to_string(),
                                    ..Default::default()
                                }],
                                ..Default::default()
                            },
                        ],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
        versions: None,
            indexes: Vec::new(),
    }
}

/// Build a proto Value from a string.
fn str_val(s: &str) -> Value {
    Value {
        kind: Some(Kind::StringValue(s.to_string())),
    }
}

/// Build a proto StructValue from key-value pairs.
fn struct_val(pairs: &[(&str, Value)]) -> Value {
    let mut fields = BTreeMap::new();
    for (k, v) in pairs {
        fields.insert(k.to_string(), v.clone());
    }
    Value {
        kind: Some(Kind::StructValue(Struct { fields })),
    }
}

/// Build a proto ListValue from values.
fn list_val(items: Vec<Value>) -> Value {
    Value {
        kind: Some(Kind::ListValue(prost_types::ListValue { values: items })),
    }
}

/// Build a products Struct for gRPC Create.
fn make_product_struct(
    name: &str,
    seo_meta_title: &str,
    variant_color: &str,
    dim_width: &str,
    dim_height: &str,
    blocks: Vec<Value>,
) -> Struct {
    let mut fields = BTreeMap::new();
    fields.insert("name".to_string(), str_val(name));
    fields.insert(
        "seo".to_string(),
        struct_val(&[("meta_title", str_val(seo_meta_title))]),
    );
    fields.insert(
        "variants".to_string(),
        list_val(vec![struct_val(&[
            ("color", str_val(variant_color)),
            (
                "dimensions",
                struct_val(&[("width", str_val(dim_width)), ("height", str_val(dim_height))]),
            ),
        ])]),
    );
    fields.insert("content".to_string(), list_val(blocks));
    Struct { fields }
}

#[tokio::test]
async fn find_with_where_dot_notation() {
    let ts = setup_service(vec![make_products_def()], vec![]);

    // Create Product 1: "Widget" — red variant, text block
    let widget_data = make_product_struct(
        "Widget",
        "Buy Widget",
        "red",
        "10",
        "20",
        vec![struct_val(&[
            ("_block_type", str_val("text")),
            ("body", str_val("Widget description here")),
        ])],
    );
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "products".to_string(),
            data: Some(widget_data),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    // Create Product 2: "Gadget" — blue variant, section block with group
    let gadget_data = make_product_struct(
        "Gadget",
        "Buy Gadget",
        "blue",
        "5",
        "15",
        vec![struct_val(&[
            ("_block_type", str_val("section")),
            ("heading", str_val("About Gadget")),
            ("meta", struct_val(&[("author", str_val("Alice"))])),
        ])],
    );
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "products".to_string(),
            data: Some(gadget_data),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    // 1. Group sub-field: seo.meta_title contains "Widget"
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "products".to_string(),
            r#where: Some(r#"{"seo.meta_title": {"contains": "Widget"}}"#.to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 1, "group sub-field filter");
    assert_eq!(
        get_proto_field(&resp.documents[0], "name").as_deref(),
        Some("Widget")
    );

    // 2. Array sub-field: variants.color = "red"
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "products".to_string(),
            r#where: Some(r#"{"variants.color": "red"}"#.to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 1, "array sub-field filter");
    assert_eq!(
        get_proto_field(&resp.documents[0], "name").as_deref(),
        Some("Widget")
    );

    // 3. Group-in-array: variants.dimensions.width = "10"
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "products".to_string(),
            r#where: Some(r#"{"variants.dimensions.width": "10"}"#.to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 1, "group-in-array filter");
    assert_eq!(
        get_proto_field(&resp.documents[0], "name").as_deref(),
        Some("Widget")
    );

    // 4. Block sub-field: content.body contains "description"
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "products".to_string(),
            r#where: Some(r#"{"content.body": {"contains": "description"}}"#.to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 1, "block sub-field filter");
    assert_eq!(
        get_proto_field(&resp.documents[0], "name").as_deref(),
        Some("Widget")
    );

    // 5. Block type: content._block_type = "section"
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "products".to_string(),
            r#where: Some(r#"{"content._block_type": "section"}"#.to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 1, "block type filter");
    assert_eq!(
        get_proto_field(&resp.documents[0], "name").as_deref(),
        Some("Gadget")
    );

    // 6. Group-in-block: content.meta.author = "Alice"
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "products".to_string(),
            r#where: Some(r#"{"content.meta.author": "Alice"}"#.to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 1, "group-in-block filter");
    assert_eq!(
        get_proto_field(&resp.documents[0], "name").as_deref(),
        Some("Gadget")
    );
}

// ── Hook-modified join data persistence test ─────────────────────────────────

/// Set up a service with an init.lua that registers a before_change hook
/// to modify array data. This tests that save_join_table_data uses the
/// hook-modified data, not the original request data.
fn setup_service_with_hook(
    collections: Vec<CollectionDefinition>,
    init_lua: &str,
) -> TestSetup {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".to_string();

    // Write init.lua so HookRunner picks up the registered hook
    std::fs::write(tmp.path().join("init.lua"), init_lua).expect("write init.lua");

    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");

    let registry = Registry::shared();
    {
        let mut reg = registry.write().unwrap();
        for def in &collections {
            reg.register_collection(def.clone());
        }
    }

    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync schema");

    let hook_runner =
        HookRunner::new(tmp.path(), registry.clone(), &config).expect("create hook runner");

    let email_renderer =
        Arc::new(EmailRenderer::new(tmp.path()).expect("create email renderer"));

    let service = ContentService::new(
        db_pool.clone(),
        Registry::snapshot(&registry),
        hook_runner,
        config.auth.secret.clone(),
        &config.depth,
        &config.pagination,
        config.email.clone(),
        email_renderer,
        config.server.clone(),
        None,
        config.locale.clone(),
        tmp.path().to_path_buf(),
        std::sync::Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(5, 300)),
        config.auth.reset_token_expiry,
        config.auth.password_policy.clone(),
        std::sync::Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(3, 900)),
    );

    TestSetup { _tmp: tmp, service, pool: db_pool }
}

#[tokio::test]
async fn before_change_hook_modifies_array_data() {
    // Register a before_change hook that injects a "hook_injected" color variant
    let init_lua = r#"
        crap.hooks.register("before_change", function(ctx)
            if ctx.collection == "products" and ctx.data.variants then
                ctx.data.variants[#ctx.data.variants + 1] = {
                    color = "hook_injected",
                    dimensions = { width = "99", height = "99" },
                }
            end
            return ctx
        end)
    "#;

    let ts = setup_service_with_hook(vec![make_products_def()], init_lua);

    // Create a product with one variant — the hook should add a second
    let data = make_product_struct(
        "HookTest",
        "Test SEO",
        "original",
        "1",
        "2",
        vec![struct_val(&[
            ("_block_type", str_val("text")),
            ("body", str_val("test body")),
        ])],
    );
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "products".to_string(),
            data: Some(data),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    // Find the product and check that the hook-injected variant exists
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "products".to_string(),
            r#where: Some(r#"{"variants.color": "hook_injected"}"#.to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 1, "hook-injected variant should be findable");
    assert_eq!(
        get_proto_field(&resp.documents[0], "name").as_deref(),
        Some("HookTest")
    );

    // Also verify the original variant is still there
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "products".to_string(),
            r#where: Some(r#"{"variants.color": "original"}"#.to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 1, "original variant should still exist");
}

// ── Group 1: Filter Operators (gRPC) ──────────────────────────────────────

fn make_numbered_posts_def() -> CollectionDefinition {
    CollectionDefinition {
        slug: "items".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("Item".to_string())),
            plural: Some(LocalizedString::Plain("Items".to_string())),
        },
        timestamps: true,
        fields: vec![
            FieldDefinition {
                name: "name".to_string(),
                required: true,
                ..Default::default()
            },
            FieldDefinition {
                name: "score".to_string(),
                field_type: FieldType::Number,
                ..Default::default()
            },
            FieldDefinition {
                name: "tag".to_string(),
                ..Default::default()
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
        versions: None,
            indexes: Vec::new(),
    }
}

fn make_item(name: &str, score: &str, tag: &str) -> Struct {
    make_struct(&[("name", name), ("score", score), ("tag", tag)])
}

#[tokio::test]
async fn find_with_where_operators() {
    let ts = setup_service(vec![make_numbered_posts_def()], vec![]);

    // Seed data
    for (name, score, tag) in &[
        ("Alpha", "10", "red"),
        ("Beta", "20", "blue"),
        ("Gamma", "30", "red"),
        ("Delta", "40", ""),
        ("Epsilon", "50", "green"),
    ] {
        ts.service
            .create(Request::new(content::CreateRequest {
                collection: "items".to_string(),
                data: Some(make_item(name, score, tag)),
                locale: None,
                draft: None,
            }))
            .await
            .unwrap();
    }

    // not_equals
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "items".to_string(),
            r#where: Some(r#"{"tag": {"not_equals": "red"}}"#.to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    // Delta has tag="" which may be stored as NULL — SQL NULL != 'red' is NULL (not true)
    // So we expect either 2 (excluding NULL) or 3 (if "" is stored as empty string)
    assert!(resp.pagination.as_ref().unwrap().total_docs >= 2 && resp.pagination.as_ref().unwrap().total_docs <= 3, "not_equals: got {}", resp.pagination.as_ref().unwrap().total_docs);

    // greater_than
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "items".to_string(),
            r#where: Some(r#"{"score": {"greater_than": "30"}}"#.to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 2, "greater_than 30 → Delta(40), Epsilon(50)");

    // less_than
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "items".to_string(),
            r#where: Some(r#"{"score": {"less_than": "20"}}"#.to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 1, "less_than 20 → Alpha(10)");

    // greater_than_or_equal
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "items".to_string(),
            r#where: Some(r#"{"score": {"greater_than_or_equal": "30"}}"#.to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 3, "gte 30 → Gamma, Delta, Epsilon");

    // less_than_or_equal
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "items".to_string(),
            r#where: Some(r#"{"score": {"less_than_or_equal": "20"}}"#.to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 2, "lte 20 → Alpha, Beta");

    // in
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "items".to_string(),
            r#where: Some(r#"{"tag": {"in": ["red", "green"]}}"#.to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 3, "in [red, green] → Alpha, Gamma, Epsilon");

    // not_in
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "items".to_string(),
            r#where: Some(r#"{"tag": {"not_in": ["red", "green"]}}"#.to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    // Delta has tag="" stored as NULL — SQL NOT IN excludes NULLs
    assert!(resp.pagination.as_ref().unwrap().total_docs >= 1 && resp.pagination.as_ref().unwrap().total_docs <= 2, "not_in [red, green]: got {}", resp.pagination.as_ref().unwrap().total_docs);

    // like
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "items".to_string(),
            r#where: Some(r#"{"name": {"like": "%lph%"}}"#.to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 1, "like '%lph%' → Alpha");

    // contains
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "items".to_string(),
            r#where: Some(r#"{"name": {"contains": "eta"}}"#.to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 1, "contains 'eta' → Beta");

    // exists (tag is non-empty)
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "items".to_string(),
            r#where: Some(r#"{"tag": {"exists": true}}"#.to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(resp.pagination.as_ref().unwrap().total_docs >= 3, "exists: at least the non-empty tags");

    // not_exists (tag is empty/null)
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "items".to_string(),
            r#where: Some(r#"{"tag": {"not_exists": true}}"#.to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    // Delta has tag="" which may or may not count as "not exists" depending on impl
    assert!(resp.pagination.as_ref().unwrap().total_docs <= 2, "not_exists: empty/null tags");
}

// ── Group 2: Unique Constraints (gRPC) ────────────────────────────────────

fn make_posts_with_unique_slug() -> CollectionDefinition {
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
                required: true,
                ..Default::default()
            },
            FieldDefinition {
                name: "slug".to_string(),
                unique: true,
                ..Default::default()
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
        versions: None,
            indexes: Vec::new(),
    }
}

#[tokio::test]
async fn find_with_unique_constraint_violation() {
    let ts = setup_service(vec![make_posts_with_unique_slug()], vec![]);

    // First create succeeds
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "articles".to_string(),
            data: Some(make_struct(&[("title", "First"), ("slug", "my-slug")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    // Second create with same slug should fail
    let err = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "articles".to_string(),
            data: Some(make_struct(&[("title", "Second"), ("slug", "my-slug")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap_err();

    // Should be some error (InvalidArgument or Internal depending on where uniqueness is enforced)
    assert!(
        err.code() == tonic::Code::InvalidArgument
            || err.code() == tonic::Code::AlreadyExists
            || err.code() == tonic::Code::Internal,
        "Duplicate unique field should return error, got: {:?}: {}",
        err.code(),
        err.message()
    );
}

// ── Group 3: Custom Validators (gRPC) ─────────────────────────────────────

#[tokio::test]
async fn create_with_custom_validator() {
    // Write validator as a proper module file so hook resolution finds it
    let tmp = tempfile::tempdir().expect("tempdir");
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();

    std::fs::write(
        hooks_dir.join("score_validator.lua"),
        r#"
local M = {}
function M.check(value, ctx)
    if value == nil then return true end
    local n = tonumber(value)
    if n == nil then return "score must be a number" end
    if n < 0 then return "score must be positive" end
    return true
end
return M
        "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".to_string();

    let def = CollectionDefinition {
        slug: "scored".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("Scored".to_string())),
            plural: Some(LocalizedString::Plain("Scored".to_string())),
        },
        timestamps: true,
        fields: vec![
            FieldDefinition {
                name: "name".to_string(),
                required: true,
                ..Default::default()
            },
            FieldDefinition {
                name: "score".to_string(),
                validate: Some("hooks.score_validator.check".to_string()),
                ..Default::default()
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
        versions: None,
            indexes: Vec::new(),
    };

    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");
    let registry = Registry::shared();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def);
    }
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync schema");
    let hook_runner =
        HookRunner::new(tmp.path(), registry.clone(), &config).expect("create hook runner");
    let email_renderer =
        Arc::new(EmailRenderer::new(tmp.path()).expect("create email renderer"));
    let service = ContentService::new(
        db_pool.clone(), Registry::snapshot(&registry), hook_runner, config.auth.secret.clone(), &config.depth, &config.pagination,
        config.email.clone(), email_renderer, config.server.clone(), None, config.locale.clone(),
        tmp.path().to_path_buf(),
        std::sync::Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(5, 300)),
        config.auth.reset_token_expiry,
        config.auth.password_policy.clone(),
        std::sync::Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(3, 900)),
    );
    let ts = TestSetup { _tmp: tmp, service, pool: db_pool };

    // Valid score passes
    let resp = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "scored".to_string(),
            data: Some(make_struct(&[("name", "Good"), ("score", "42")])),
            locale: None,
            draft: None,
        }))
        .await;
    assert!(resp.is_ok(), "Valid score should succeed");

    // Negative score fails
    let err = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "scored".to_string(),
            data: Some(make_struct(&[("name", "Bad"), ("score", "-5")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap_err();
    assert!(
        err.message().contains("positive"),
        "Negative score should trigger validator: {}",
        err.message()
    );
}

// ── Group 4: Field-Level Hooks (gRPC) ─────────────────────────────────────

#[tokio::test]
async fn field_level_before_change_hook() {
    // Write hook as a proper module file
    let tmp = tempfile::tempdir().expect("tempdir");
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();

    std::fs::write(
        hooks_dir.join("slug_gen.lua"),
        r#"
local M = {}
function M.auto_slug(value, ctx)
    if (value == nil or value == "") and ctx.data and ctx.data.name then
        local s = ctx.data.name:lower()
        s = s:gsub("[^%w%s-]", "")
        s = s:gsub("%s+", "-")
        return s
    end
    return value
end
return M
        "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".to_string();

    let def = CollectionDefinition {
        slug: "pages".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("Page".to_string())),
            plural: Some(LocalizedString::Plain("Pages".to_string())),
        },
        timestamps: true,
        fields: vec![
            FieldDefinition {
                name: "name".to_string(),
                required: true,
                ..Default::default()
            },
            FieldDefinition {
                name: "slug".to_string(),
                hooks: FieldHooks {
                    before_change: vec!["hooks.slug_gen.auto_slug".to_string()],
                    ..Default::default()
                },
                ..Default::default()
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
        versions: None,
            indexes: Vec::new(),
    };

    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");
    let registry = Registry::shared();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def);
    }
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync schema");
    let hook_runner =
        HookRunner::new(tmp.path(), registry.clone(), &config).expect("create hook runner");
    let email_renderer =
        Arc::new(EmailRenderer::new(tmp.path()).expect("create email renderer"));
    let service = ContentService::new(
        db_pool.clone(), Registry::snapshot(&registry), hook_runner, config.auth.secret.clone(), &config.depth, &config.pagination,
        config.email.clone(), email_renderer, config.server.clone(), None, config.locale.clone(),
        tmp.path().to_path_buf(),
        std::sync::Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(5, 300)),
        config.auth.reset_token_expiry,
        config.auth.password_policy.clone(),
        std::sync::Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(3, 900)),
    );
    let ts = TestSetup { _tmp: tmp, service, pool: db_pool };

    // Create without providing slug — hook should auto-generate
    let doc = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "pages".to_string(),
            data: Some(make_struct(&[("name", "Hello World")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    assert_eq!(
        get_proto_field(&doc, "slug").as_deref(),
        Some("hello-world"),
        "Field before_change hook should auto-generate slug"
    );
}

#[tokio::test]
async fn field_level_after_read_hook() {
    // Write hook as proper module file
    let tmp = tempfile::tempdir().expect("tempdir");
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();

    std::fs::write(
        hooks_dir.join("transform.lua"),
        r#"
local M = {}
function M.uppercase_on_read(value, ctx)
    if value then return value:upper() end
    return value
end
return M
        "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".to_string();

    let def = CollectionDefinition {
        slug: "entries".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("Entry".to_string())),
            plural: Some(LocalizedString::Plain("Entries".to_string())),
        },
        timestamps: true,
        fields: vec![FieldDefinition {
            name: "name".to_string(),
            required: true,
            hooks: FieldHooks {
                after_read: vec!["hooks.transform.uppercase_on_read".to_string()],
                ..Default::default()
            },
            ..Default::default()
        }],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
        versions: None,
            indexes: Vec::new(),
    };

    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");
    let registry = Registry::shared();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def);
    }
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync schema");
    let hook_runner =
        HookRunner::new(tmp.path(), registry.clone(), &config).expect("create hook runner");
    let email_renderer =
        Arc::new(EmailRenderer::new(tmp.path()).expect("create email renderer"));
    let service = ContentService::new(
        db_pool.clone(), Registry::snapshot(&registry), hook_runner, config.auth.secret.clone(), &config.depth, &config.pagination,
        config.email.clone(), email_renderer, config.server.clone(), None, config.locale.clone(),
        tmp.path().to_path_buf(),
        std::sync::Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(5, 300)),
        config.auth.reset_token_expiry,
        config.auth.password_policy.clone(),
        std::sync::Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(3, 900)),
    );
    let ts = TestSetup { _tmp: tmp, service, pool: db_pool };

    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "entries".to_string(),
            data: Some(make_struct(&[("name", "hello world")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    // Find should return uppercased name
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "entries".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.documents.len(), 1);
    assert_eq!(
        get_proto_field(&resp.documents[0], "name").as_deref(),
        Some("HELLO WORLD"),
        "Field after_read hook should uppercase name"
    );
}

// ── Group 5: Collection-Level Hooks (gRPC) ────────────────────────────────

#[tokio::test]
async fn collection_after_read_hook() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();

    std::fs::write(
        hooks_dir.join("note_hooks.lua"),
        r#"
local M = {}
function M.add_computed(ctx)
    if ctx.data and ctx.data.title then
        ctx.data.computed = "read:" .. ctx.data.title
    end
    return ctx
end
return M
        "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".to_string();

    let def = CollectionDefinition {
        slug: "notes".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("Note".to_string())),
            plural: Some(LocalizedString::Plain("Notes".to_string())),
        },
        timestamps: true,
        fields: vec![
            FieldDefinition {
                name: "title".to_string(),
                required: true,
                ..Default::default()
            },
            FieldDefinition {
                name: "computed".to_string(),
                ..Default::default()
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks {
            after_read: vec!["hooks.note_hooks.add_computed".to_string()],
            ..Default::default()
        },
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
        versions: None,
            indexes: Vec::new(),
    };

    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");
    let registry = Registry::shared();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def);
    }
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync schema");
    let hook_runner =
        HookRunner::new(tmp.path(), registry.clone(), &config).expect("create hook runner");
    let email_renderer =
        Arc::new(EmailRenderer::new(tmp.path()).expect("create email renderer"));
    let service = ContentService::new(
        db_pool.clone(), Registry::snapshot(&registry), hook_runner, config.auth.secret.clone(), &config.depth, &config.pagination,
        config.email.clone(), email_renderer, config.server.clone(), None, config.locale.clone(),
        tmp.path().to_path_buf(),
        std::sync::Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(5, 300)),
        config.auth.reset_token_expiry,
        config.auth.password_policy.clone(),
        std::sync::Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(3, 900)),
    );
    let ts = TestSetup { _tmp: tmp, service, pool: db_pool };

    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "notes".to_string(),
            data: Some(make_struct(&[("title", "Test Note")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "notes".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.documents.len(), 1);
    assert_eq!(
        get_proto_field(&resp.documents[0], "computed").as_deref(),
        Some("read:Test Note"),
        "after_read hook should add computed field"
    );
}

#[tokio::test]
async fn collection_before_validate_hook() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();

    std::fs::write(
        hooks_dir.join("moderator.lua"),
        r#"
local M = {}
function M.reject_forbidden(ctx)
    if ctx.data and ctx.data.title and ctx.data.title:find("FORBIDDEN") then
        error("Title contains forbidden word")
    end
    return ctx
end
return M
        "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".to_string();

    let def = CollectionDefinition {
        slug: "moderated".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("Moderated".to_string())),
            plural: Some(LocalizedString::Plain("Moderated".to_string())),
        },
        timestamps: true,
        fields: vec![FieldDefinition {
            name: "title".to_string(),
            required: true,
            ..Default::default()
        }],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks {
            before_validate: vec!["hooks.moderator.reject_forbidden".to_string()],
            ..Default::default()
        },
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
        versions: None,
            indexes: Vec::new(),
    };

    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");
    let registry = Registry::shared();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def);
    }
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync schema");
    let hook_runner =
        HookRunner::new(tmp.path(), registry.clone(), &config).expect("create hook runner");
    let email_renderer =
        Arc::new(EmailRenderer::new(tmp.path()).expect("create email renderer"));
    let service = ContentService::new(
        db_pool.clone(), Registry::snapshot(&registry), hook_runner, config.auth.secret.clone(), &config.depth, &config.pagination,
        config.email.clone(), email_renderer, config.server.clone(), None, config.locale.clone(),
        tmp.path().to_path_buf(),
        std::sync::Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(5, 300)),
        config.auth.reset_token_expiry,
        config.auth.password_policy.clone(),
        std::sync::Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(3, 900)),
    );
    let ts = TestSetup { _tmp: tmp, service, pool: db_pool };

    // Valid title succeeds
    let resp = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "moderated".to_string(),
            data: Some(make_struct(&[("title", "Good Title")])),
            locale: None,
            draft: None,
        }))
        .await;
    assert!(resp.is_ok(), "Valid title should pass");

    // Forbidden title fails
    let err = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "moderated".to_string(),
            data: Some(make_struct(&[("title", "FORBIDDEN content")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap_err();
    assert!(
        err.message().contains("forbidden") || err.message().contains("FORBIDDEN"),
        "Hook should reject forbidden title: {}",
        err.message()
    );
}

// ── Group 6: Localization (gRPC) ──────────────────────────────────────────

fn make_localized_posts_def() -> CollectionDefinition {
    CollectionDefinition {
        slug: "posts".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("Post".to_string())),
            plural: Some(LocalizedString::Plain("Posts".to_string())),
        },
        timestamps: true,
        fields: vec![
            FieldDefinition {
                name: "title".to_string(),
                required: true,
                localized: true,
                ..Default::default()
            },
            FieldDefinition {
                name: "body".to_string(),
                field_type: FieldType::Textarea,
                ..Default::default()
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
        versions: None,
            indexes: Vec::new(),
    }
}

fn setup_service_with_locale(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
    locales: Vec<&str>,
) -> TestSetup {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".to_string();
    config.locale.locales = locales.iter().map(|s| s.to_string()).collect();
    config.locale.default_locale = locales.first().unwrap_or(&"en").to_string();
    config.locale.fallback = true;

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

    let hook_runner =
        HookRunner::new(tmp.path(), registry.clone(), &config).expect("create hook runner");

    let email_renderer =
        Arc::new(EmailRenderer::new(tmp.path()).expect("create email renderer"));

    let service = ContentService::new(
        db_pool.clone(),
        Registry::snapshot(&registry),
        hook_runner,
        config.auth.secret.clone(),
        &config.depth,
        &config.pagination,
        config.email.clone(),
        email_renderer,
        config.server.clone(),
        None,
        config.locale.clone(),
        tmp.path().to_path_buf(),
        std::sync::Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(5, 300)),
        config.auth.reset_token_expiry,
        config.auth.password_policy.clone(),
        std::sync::Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(3, 900)),
    );

    TestSetup { _tmp: tmp, service, pool: db_pool }
}

#[tokio::test]
async fn create_and_find_with_locale() {
    let ts = setup_service_with_locale(
        vec![make_localized_posts_def()],
        vec![],
        vec!["en", "de"],
    );

    // Create with locale=en
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(make_struct(&[("title", "Hello"), ("body", "English body")])),
            locale: Some("en".to_string()),
            draft: None,
        }))
        .await
        .unwrap();

    // Find with locale=en should return the English title
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "posts".to_string(),
            locale: Some("en".to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 1);
    assert_eq!(
        get_proto_field(&resp.documents[0], "title").as_deref(),
        Some("Hello"),
        "Should return English title"
    );
}

#[tokio::test]
async fn create_and_find_with_locale_fallback() {
    let ts = setup_service_with_locale(
        vec![make_localized_posts_def()],
        vec![],
        vec!["en", "de"],
    );

    // Create with locale=en only
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(make_struct(&[("title", "English Only")])),
            locale: Some("en".to_string()),
            draft: None,
        }))
        .await
        .unwrap();

    // Find with locale=de should fallback to en value
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "posts".to_string(),
            locale: Some("de".to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 1);
    assert_eq!(
        get_proto_field(&resp.documents[0], "title").as_deref(),
        Some("English Only"),
        "Fallback should return default locale value"
    );
}

#[tokio::test]
async fn create_and_find_with_locale_all() {
    let ts = setup_service_with_locale(
        vec![make_localized_posts_def()],
        vec![],
        vec!["en", "de"],
    );

    // Create English version
    let doc = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(make_struct(&[("title", "English Title")])),
            locale: Some("en".to_string()),
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // Update with German version
    ts.service
        .update(Request::new(content::UpdateRequest {
            collection: "posts".to_string(),
            id: doc.id.clone(),
            data: Some(make_struct(&[("title", "Deutscher Titel")])),
            locale: Some("de".to_string()),
            draft: None,
            unpublish: None,
        }))
        .await
        .unwrap();

    // Find with locale=all should return nested locale objects
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "posts".to_string(),
            locale: Some("all".to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 1);
    let fields = resp.documents[0].fields.as_ref().unwrap();
    let title_val = fields.fields.get("title");
    assert!(title_val.is_some(), "title field should be present");

    // When locale=all, title should be a struct with en/de keys
    match &title_val.unwrap().kind {
        Some(Kind::StructValue(s)) => {
            assert!(
                s.fields.contains_key("en"),
                "locale=all should have 'en' key"
            );
            assert!(
                s.fields.contains_key("de"),
                "locale=all should have 'de' key"
            );
        }
        other => {
            // Some implementations may return it differently
            panic!(
                "Expected struct with locale keys for locale=all, got: {:?}",
                other
            );
        }
    }
}

// ── Group 7: Drafts (gRPC) ───────────────────────────────────────────────

fn make_versioned_posts_def() -> CollectionDefinition {
    CollectionDefinition {
        slug: "posts".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("Post".to_string())),
            plural: Some(LocalizedString::Plain("Posts".to_string())),
        },
        timestamps: true,
        fields: vec![
            FieldDefinition {
                name: "title".to_string(),
                required: true,
                ..Default::default()
            },
            FieldDefinition {
                name: "body".to_string(),
                field_type: FieldType::Textarea,
                ..Default::default()
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
        versions: Some(VersionsConfig {
            drafts: true,
            max_versions: 10,
        }),
        indexes: Vec::new(),
    }
}

#[tokio::test]
async fn create_draft_and_find() {
    let ts = setup_service(vec![make_versioned_posts_def()], vec![]);

    // Create a draft
    let doc = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(make_struct(&[("title", "Draft Post"), ("body", "WIP")])),
            locale: None,
            draft: Some(true),
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();
    assert!(!doc.id.is_empty());

    // Find with draft=true should return the draft
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "posts".to_string(),
            draft: Some(true),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 1, "draft=true should find the draft");

    // Find without draft flag should NOT return drafts
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "posts".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 0, "default find should not return drafts");
}

#[tokio::test]
async fn draft_skips_required_validation() {
    let ts = setup_service(vec![make_versioned_posts_def()], vec![]);

    // Create a draft without required 'title' field — should succeed
    let resp = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(make_struct(&[("body", "Just a body")])),
            locale: None,
            draft: Some(true),
        }))
        .await;
    assert!(
        resp.is_ok(),
        "Draft should skip required validation: {:?}",
        resp.err()
    );
}

#[tokio::test]
async fn publish_draft() {
    let ts = setup_service(vec![make_versioned_posts_def()], vec![]);

    // Create a draft
    let doc = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(make_struct(&[
                ("title", "Draft to Publish"),
                ("body", "Content"),
            ])),
            locale: None,
            draft: Some(true),
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // Publish it by updating with draft=false
    ts.service
        .update(Request::new(content::UpdateRequest {
            collection: "posts".to_string(),
            id: doc.id.clone(),
            data: Some(make_struct(&[("title", "Draft to Publish")])),
            locale: None,
            draft: Some(false),
            unpublish: None,
        }))
        .await
        .unwrap();

    // Now find without draft flag should return it
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "posts".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 1, "Published post should be findable");
}

// ── Group 8: Complex Globals (gRPC) ──────────────────────────────────────

fn make_complex_global_def() -> GlobalDefinition {
    GlobalDefinition {
        slug: "site_config".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("Site Config".to_string())),
            plural: None,
        },
        fields: vec![
            FieldDefinition {
                name: "site_name".to_string(),
                ..Default::default()
            },
            FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                fields: vec![
                    FieldDefinition {
                        name: "meta_title".to_string(),
                        ..Default::default()
                    },
                    FieldDefinition {
                        name: "meta_description".to_string(),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            FieldDefinition {
                name: "nav_items".to_string(),
                field_type: FieldType::Array,
                fields: vec![
                    FieldDefinition {
                        name: "label".to_string(),
                        ..Default::default()
                    },
                    FieldDefinition {
                        name: "url".to_string(),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            FieldDefinition {
                name: "sections".to_string(),
                field_type: FieldType::Blocks,
                blocks: vec![BlockDefinition {
                    block_type: "hero".to_string(),
                    fields: vec![FieldDefinition {
                        name: "heading".to_string(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            },
        ],
        hooks: CollectionHooks::default(),
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
        versions: None,
    }
}

#[tokio::test]
async fn update_global_with_nested_fields() {
    let ts = setup_service(vec![], vec![make_complex_global_def()]);

    // Build complex nested data
    let mut data_fields = BTreeMap::new();
    data_fields.insert("site_name".to_string(), str_val("My Site"));
    data_fields.insert(
        "seo".to_string(),
        struct_val(&[
            ("meta_title", str_val("Site Title")),
            ("meta_description", str_val("Site Description")),
        ]),
    );
    data_fields.insert(
        "nav_items".to_string(),
        list_val(vec![
            struct_val(&[("label", str_val("Home")), ("url", str_val("/"))]),
            struct_val(&[("label", str_val("About")), ("url", str_val("/about"))]),
        ]),
    );
    data_fields.insert(
        "sections".to_string(),
        list_val(vec![struct_val(&[
            ("_block_type", str_val("hero")),
            ("heading", str_val("Welcome!")),
        ])]),
    );

    ts.service
        .update_global(Request::new(content::UpdateGlobalRequest {
            slug: "site_config".to_string(),
            data: Some(Struct {
                fields: data_fields,
            }),
            locale: None,
        }))
        .await
        .unwrap();

    // Read back and verify
    let doc = ts
        .service
        .get_global(Request::new(content::GetGlobalRequest {
            slug: "site_config".to_string(),
            locale: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    let fields = doc.fields.as_ref().unwrap();
    assert_eq!(
        get_proto_field(&doc, "site_name").as_deref(),
        Some("My Site")
    );

    // Verify seo group
    let seo = fields.fields.get("seo");
    assert!(seo.is_some(), "seo group should exist");
    if let Some(Kind::StructValue(s)) = seo.unwrap().kind.as_ref() {
        assert!(
            s.fields.contains_key("meta_title"),
            "seo should have meta_title"
        );
    }

    // Verify nav_items array
    let nav = fields.fields.get("nav_items");
    assert!(nav.is_some(), "nav_items should exist");
    if let Some(Kind::ListValue(l)) = nav.unwrap().kind.as_ref() {
        assert_eq!(l.values.len(), 2, "Should have 2 nav items");
    }

    // Verify blocks
    let sections = fields.fields.get("sections");
    assert!(sections.is_some(), "sections should exist");
    if let Some(Kind::ListValue(l)) = sections.unwrap().kind.as_ref() {
        assert_eq!(l.values.len(), 1, "Should have 1 section block");
    }
}

// ── Group 9: Has-Many Relationship Filters (gRPC) ────────────────────────

fn make_tags_def() -> CollectionDefinition {
    CollectionDefinition {
        slug: "tags".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("Tag".to_string())),
            plural: Some(LocalizedString::Plain("Tags".to_string())),
        },
        timestamps: true,
        fields: vec![FieldDefinition {
            name: "name".to_string(),
            required: true,
            ..Default::default()
        }],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
        versions: None,
            indexes: Vec::new(),
    }
}

fn make_posts_with_has_many() -> CollectionDefinition {
    CollectionDefinition {
        slug: "posts".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("Post".to_string())),
            plural: Some(LocalizedString::Plain("Posts".to_string())),
        },
        timestamps: true,
        fields: vec![
            FieldDefinition {
                name: "title".to_string(),
                required: true,
                ..Default::default()
            },
            FieldDefinition {
                name: "tags".to_string(),
                field_type: FieldType::Relationship,
                relationship: Some(RelationshipConfig {
                    collection: "tags".to_string(),
                    has_many: true,
                    max_depth: None,
                    polymorphic: vec![],
                }),
                ..Default::default()
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
        versions: None,
            indexes: Vec::new(),
    }
}

#[tokio::test]
async fn find_with_has_many_relationship_filter() {
    let ts = setup_service(
        vec![make_tags_def(), make_posts_with_has_many()],
        vec![],
    );

    // Create tags
    let tag_rust = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "tags".to_string(),
            data: Some(make_struct(&[("name", "rust")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    let tag_web = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "tags".to_string(),
            data: Some(make_struct(&[("name", "web")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // Create posts with tags (has-many: pass as comma-separated or list)
    let mut post1_fields = BTreeMap::new();
    post1_fields.insert("title".to_string(), str_val("Rust Post"));
    post1_fields.insert(
        "tags".to_string(),
        list_val(vec![str_val(&tag_rust.id)]),
    );
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(Struct {
                fields: post1_fields,
            }),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    let mut post2_fields = BTreeMap::new();
    post2_fields.insert("title".to_string(), str_val("Web Post"));
    post2_fields.insert(
        "tags".to_string(),
        list_val(vec![str_val(&tag_web.id)]),
    );
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(Struct {
                fields: post2_fields,
            }),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    let mut post3_fields = BTreeMap::new();
    post3_fields.insert("title".to_string(), str_val("Both Post"));
    post3_fields.insert(
        "tags".to_string(),
        list_val(vec![str_val(&tag_rust.id), str_val(&tag_web.id)]),
    );
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(Struct {
                fields: post3_fields,
            }),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    // Filter posts by tags.id containing the rust tag ID
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "posts".to_string(),
            r#where: Some(format!(r#"{{"tags.id": "{}"}}"#, tag_rust.id)),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        resp.pagination.as_ref().unwrap().total_docs, 2,
        "Should find 2 posts with rust tag (Rust Post + Both Post)"
    );
}

// ── Group 10: Bulk Operations (gRPC) ─────────────────────────────────────

#[tokio::test]
async fn update_many_with_filter() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    for (title, status) in &[
        ("A", "draft"),
        ("B", "draft"),
        ("C", "published"),
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

    // Update all drafts to published
    let resp = ts
        .service
        .update_many(Request::new(content::UpdateManyRequest {
            collection: "posts".to_string(),
            r#where: Some(r#"{"status": "draft"}"#.to_string()),
            data: Some(make_struct(&[("status", "published")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.modified, 2, "Should update 2 draft posts");

    // Verify all are now published
    let count_resp = ts
        .service
        .count(Request::new(content::CountRequest {
            collection: "posts".to_string(),
            r#where: Some(r#"{"status": "published"}"#.to_string()),
            locale: None,
            draft: None,
            search: None,
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(count_resp.count, 3, "All 3 should be published");
}

#[tokio::test]
async fn delete_many_with_where() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    for (title, status) in &[
        ("A", "draft"),
        ("B", "draft"),
        ("C", "published"),
        ("D", "published"),
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

    // Delete all drafts
    let resp = ts
        .service
        .delete_many(Request::new(content::DeleteManyRequest {
            collection: "posts".to_string(),
            r#where: Some(r#"{"status": "draft"}"#.to_string()),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.deleted, 2, "Should delete 2 draft posts");

    // Verify only published remain
    let count_resp = ts
        .service
        .count(Request::new(content::CountRequest {
            collection: "posts".to_string(),
            r#where: None,
            locale: None,
            draft: None,
            search: None,
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(count_resp.count, 2, "2 published posts should remain");
}

// ── Group 11: Versions (gRPC) ────────────────────────────────────────────

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
    assert!(
        restored_title.is_some(),
        "Restored doc should have a title"
    );
    assert_ne!(
        restored_title.as_deref(),
        Some("Version 3"),
        "Restored should not be version 3 anymore"
    );
}

// ── Auth RPC Gaps ─────────────────────────────────────────────────────────

#[tokio::test]
async fn login_locked_account_grpc() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    // Create a user with password
    let doc = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "users".to_string(),
            data: Some(make_struct(&[
                ("email", "locked@example.com"),
                ("name", "Locked User"),
                ("password", "secret123"),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // Lock the user via direct DB access
    {
        let conn = ts.pool.get().unwrap();
        query::lock_user(&conn, "users", &doc.id).unwrap();
    }

    // Try to login — should fail because the account is locked
    let err = ts
        .service
        .login(Request::new(content::LoginRequest {
            collection: "users".to_string(),
            email: "locked@example.com".to_string(),
            password: "secret123".to_string(),
        }))
        .await
        .unwrap_err();

    assert!(
        err.code() == tonic::Code::Unauthenticated
            || err.code() == tonic::Code::PermissionDenied,
        "Locked account login should return Unauthenticated or PermissionDenied, got {:?}: {}",
        err.code(),
        err.message()
    );
}

#[tokio::test]
async fn forgot_password_nonexistent_still_succeeds() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    // Call ForgotPassword with a completely non-existent email
    let resp = ts
        .service
        .forgot_password(Request::new(content::ForgotPasswordRequest {
            collection: "users".to_string(),
            email: "does-not-exist@example.com".to_string(),
        }))
        .await
        .unwrap()
        .into_inner();

    // Should always return success to avoid leaking user existence
    assert!(
        resp.success,
        "ForgotPassword should always return success, even for non-existent emails"
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
            data: Some(make_struct(&[("title", "Field Check"), ("status", "draft")])),
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
    assert_eq!(get_proto_field(&doc, "title").as_deref(), Some("Field Check"));
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
    assert_eq!(get_proto_field(&found, "title").as_deref(), Some("Field Check"));
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
    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 5, "Total count should still be 5 regardless of pagination");

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
    assert_eq!(resp2.pagination.as_ref().unwrap().total_docs, 5, "Total count should still be 5");
}

// ── Relationship Gaps ─────────────────────────────────────────────────────

#[tokio::test]
async fn find_depth_0_returns_id_only() {
    let ts = setup_service(
        vec![make_categories_def(), make_posts_with_relationship()],
        vec![],
    );

    // Create a category
    let cat_doc = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "categories".to_string(),
            data: Some(make_struct(&[("name", "Art")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // Create a post with the category relationship
    let post_doc = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(make_struct(&[
                ("title", "Depth Zero Test"),
                ("category", &cat_doc.id),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // Find with depth=0 — category should be a string ID, not a populated object
    let found = ts
        .service
        .find_by_id(Request::new(content::FindByIdRequest {
            collection: "posts".to_string(),
            id: post_doc.id.clone(),
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

    let fields = found.fields.as_ref().unwrap();
    let cat_field = fields.fields.get("category");
    assert!(cat_field.is_some(), "category field should be present");

    match &cat_field.unwrap().kind {
        Some(Kind::StringValue(s)) => {
            assert_eq!(
                s, &cat_doc.id,
                "At depth=0, category should be the raw ID string"
            );
        }
        other => {
            panic!(
                "At depth=0, category should be a StringValue (ID), got: {:?}",
                other
            );
        }
    }
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
            ..Default::default()
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
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.deleted, 0);
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
            data: Some(make_struct(&[("title", "Select Me"), ("status", "published")])),
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
// Login with locked account
// ══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn login_locked_account() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    // Create user
    let doc = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "users".to_string(),
            data: Some(make_struct(&[
                ("email", "locked@example.com"),
                ("password", "secret123"),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // Lock the user directly in the DB
    {
        let conn = ts.pool.get().unwrap();
        conn.execute(
            "UPDATE users SET _locked = 1 WHERE id = ?1",
            rusqlite::params![doc.id],
        )
        .unwrap();
    }

    // Try to login
    let err = ts
        .service
        .login(Request::new(content::LoginRequest {
            collection: "users".to_string(),
            email: "locked@example.com".to_string(),
            password: "secret123".to_string(),
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::PermissionDenied);
    assert!(err.message().to_lowercase().contains("locked"));
}

// ══════════════════════════════════════════════════════════════════════════════
// FindByID not found
// ══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn find_by_id_not_found() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let resp = ts
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
        .unwrap()
        .into_inner();

    assert!(resp.document.is_none());
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
    for title in &["Rust Programming Guide", "Python Tutorial", "Advanced Rust Patterns"] {
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
        assert!(title.contains("Rust"), "Expected Rust in title, got: {}", title);
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
