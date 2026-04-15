//! Integration tests for the gRPC ContentService via the ContentApi trait.
//!
//! These tests construct a ContentService directly (no network) and call
//! trait methods with tonic::Request objects to exercise the full RPC path.
//!
//! This file covers: basic CRUD, globals, list/describe endpoints.
//! Auth tests → grpc_auth.rs
//! Query/filter/hook/depth tests → grpc_query.rs
//! Locale/draft/version/bulk/FTS tests → grpc_hooks_locale.rs

use std::collections::BTreeMap;
use std::sync::Arc;

use prost_types::{ListValue, Struct, Value, value::Kind};
use tonic::Request;

use crap_cms::api::content;
use crap_cms::api::content::content_api_server::ContentApi;
use crap_cms::api::handlers::{ContentService, ContentServiceDeps};
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

fn make_global_def() -> GlobalDefinition {
    let mut def = GlobalDefinition::new("settings");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Settings".to_string())),
        plural: None,
    };
    def.fields = vec![FieldDefinition::builder("site_name", FieldType::Text).build()];
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
    setup_service_inner(collections, globals, None)
}

/// Build a `ContentService` whose registry is loaded from a Lua fixture dir,
/// so tests can exercise real access hooks and Lua-defined globals against
/// the gRPC surface. The HookRunner uses the fixture dir as its config dir
/// so hooks resolve via package paths.
#[allow(dead_code)]
fn setup_service_with_fixture(fixture_dir: &std::path::Path) -> TestSetup {
    setup_service_inner(Vec::new(), Vec::new(), Some(fixture_dir.to_path_buf()))
}

fn setup_service_inner(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
    fixture_dir: Option<std::path::PathBuf>,
) -> TestSetup {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();

    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");

    let (registry, hook_config_dir) = match fixture_dir.as_deref() {
        Some(fd) => {
            let reg = crap_cms::hooks::init_lua(fd, &config).expect("init lua from fixture");
            (reg, fd.to_path_buf())
        }
        None => (Registry::shared(), tmp.path().to_path_buf()),
    };

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
        .config_dir(&hook_config_dir)
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
    }
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
            trash: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .expect("Document not found");

    assert_eq!(found.id, doc.id);
    assert_eq!(
        get_proto_field(&found, "title").as_deref(),
        Some("Test Post")
    );
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
    assert_eq!(
        get_proto_field(&updated, "title").as_deref(),
        Some("Updated")
    );
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
            force_hard_delete: false,
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
            trash: None,
        }))
        .await
        .unwrap_err();

    assert_eq!(found.code(), tonic::Code::NotFound);
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
            r#where: Some(r#"{"or": [{"title": "Alpha"}, {"title": "Gamma"}]}"#.to_string()),
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

    assert_eq!(
        get_proto_field(&doc, "site_name").as_deref(),
        Some("My CMS")
    );
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
    let ts = setup_service(
        vec![make_posts_def(), make_users_def()],
        vec![make_global_def()],
    );

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

// ── Deep nesting validation via gRPC ────────────────────────────────────

fn str_val(s: &str) -> Value {
    Value {
        kind: Some(Kind::StringValue(s.to_string())),
    }
}

fn struct_val(pairs: &[(&str, Value)]) -> Value {
    let mut fields = BTreeMap::new();
    for (k, v) in pairs {
        fields.insert(k.to_string(), v.clone());
    }
    Value {
        kind: Some(Kind::StructValue(Struct { fields })),
    }
}

fn list_val(items: Vec<Value>) -> Value {
    Value {
        kind: Some(Kind::ListValue(ListValue { values: items })),
    }
}

fn make_nested_array_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("nested_test");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Nested Test".to_string())),
        plural: Some(LocalizedString::Plain("Nested Tests".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("name", FieldType::Text)
            .required(true)
            .build(),
        // Array > Tabs > Row > required fields (team_members pattern)
        FieldDefinition::builder("team_members", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("tabs", FieldType::Tabs)
                    .tabs(vec![FieldTab::new(
                        "Personal",
                        vec![
                            FieldDefinition::builder("name_row", FieldType::Row)
                                .fields(vec![
                                    FieldDefinition::builder("first_name", FieldType::Text)
                                        .required(true)
                                        .build(),
                                    FieldDefinition::builder("last_name", FieldType::Text)
                                        .required(true)
                                        .build(),
                                ])
                                .build(),
                        ],
                    )])
                    .build(),
            ])
            .build(),
    ];
    def
}

#[tokio::test]
async fn grpc_create_rejects_empty_required_in_nested_array() {
    let ts = setup_service(vec![make_nested_array_def()], vec![]);

    // Row with empty required fields inside Array > Tabs > Row
    let row = struct_val(&[("first_name", str_val("")), ("last_name", str_val(""))]);

    let mut fields = BTreeMap::new();
    fields.insert("name".to_string(), str_val("Test"));
    fields.insert("team_members".to_string(), list_val(vec![row]));

    let err = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "nested_test".to_string(),
            data: Some(Struct { fields }),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap_err();

    assert_eq!(
        err.code(),
        tonic::Code::InvalidArgument,
        "gRPC create must reject empty required fields inside Array > Tabs > Row"
    );
    let msg = err.message().to_string();
    assert!(
        msg.contains("first_name") || msg.contains("required"),
        "Error should reference the nested required field: {}",
        msg
    );
}

// ── Globals access control (gRPC surface) ───────────────────────────────
//
// Regression for the gRPC surface of the globals read-access gate. Service-
// layer coverage lives in `tests/hook_lifecycle_globals.rs`; this test
// asserts the translation: a `ServiceError::AccessDenied` becomes
// `tonic::Code::PermissionDenied` at the gRPC boundary. Uses the same Lua
// fixture as the admin HTTP access-denied test.
fn admin_globals_access_fixture() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/admin_globals_access")
}

#[tokio::test]
async fn grpc_global_read_access_denied_returns_permission_denied() {
    let ts = setup_service_with_fixture(&admin_globals_access_fixture());

    // Create an editor user (role = "editor"), then log them in to obtain a
    // Bearer token. The access hook `hooks.access.admin_only` only admits
    // `ctx.user.role == "admin"`, so this user must be denied.
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "users".to_string(),
            data: Some(make_struct(&[
                ("email", "editor-grpc@test.com"),
                ("password", "password123"),
                ("role", "editor"),
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
            email: "editor-grpc@test.com".to_string(),
            password: "password123".to_string(),
        }))
        .await
        .unwrap()
        .into_inner()
        .token;

    let mut req = Request::new(content::GetGlobalRequest {
        slug: "restricted_settings".to_string(),
        locale: None,
    });
    req.metadata_mut().insert(
        "authorization",
        format!("Bearer {}", token).parse().unwrap(),
    );

    let err = ts
        .service
        .get_global(req)
        .await
        .expect_err("non-admin GetGlobal must be denied");
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
}

#[tokio::test]
async fn grpc_create_accepts_valid_nested_array() {
    let ts = setup_service(vec![make_nested_array_def()], vec![]);

    let row = struct_val(&[
        ("first_name", str_val("Jane")),
        ("last_name", str_val("Doe")),
    ]);

    let mut fields = BTreeMap::new();
    fields.insert("name".to_string(), str_val("Test"));
    fields.insert("team_members".to_string(), list_val(vec![row]));

    let resp = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "nested_test".to_string(),
            data: Some(Struct { fields }),
            locale: None,
            draft: None,
        }))
        .await
        .expect("Valid nested data should be accepted");

    assert!(
        !resp.get_ref().document.as_ref().unwrap().id.is_empty(),
        "Created document should have an ID"
    );
}
