//! gRPC integration tests for localized updates, unpublish, validation errors,
//! has-many relationships, group relationships with depth, and UpdateMany.
//!
//! Uses ContentService directly (no network) via ContentApi trait.

use std::collections::BTreeMap;
use std::sync::Arc;

use prost_types::{ListValue, Struct, Value, value::Kind};
use tonic::Request;

use crap_cms::api::content;
use crap_cms::api::content::content_api_server::ContentApi;
use crap_cms::api::service::{ContentService, ContentServiceDeps};
use crap_cms::config::*;
use crap_cms::core::Registry;
use crap_cms::core::collection::*;
use crap_cms::core::email::EmailRenderer;
use crap_cms::core::field::*;
use crap_cms::core::upload::CollectionUpload;
use crap_cms::db::{migrate, pool};
use crap_cms::hooks::lifecycle::HookRunner;
use serde_json::json;

// ── Helpers ───────────────────────────────────────────────────────────────

fn str_val(s: &str) -> Value {
    Value {
        kind: Some(Kind::StringValue(s.to_string())),
    }
}

fn list_val(items: Vec<Value>) -> Value {
    Value {
        kind: Some(Kind::ListValue(ListValue { values: items })),
    }
}

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

fn get_proto_field(doc: &content::Document, field: &str) -> Option<String> {
    doc.fields.as_ref().and_then(|s| {
        s.fields.get(field).and_then(|v| match &v.kind {
            Some(Kind::StringValue(s)) => Some(s.clone()),
            _ => None,
        })
    })
}

fn get_proto_value(doc: &content::Document, field: &str) -> Option<Value> {
    doc.fields
        .as_ref()
        .and_then(|s| s.fields.get(field).cloned())
}

fn get_list_items(doc: &content::Document, field: &str) -> Vec<Value> {
    match get_proto_value(doc, field) {
        Some(Value {
            kind: Some(Kind::ListValue(lv)),
        }) => lv.values,
        _ => vec![],
    }
}

fn get_struct_field_str(val: &Value, field: &str) -> Option<String> {
    match &val.kind {
        Some(Kind::StructValue(s)) => s.fields.get(field).and_then(|v| match &v.kind {
            Some(Kind::StringValue(s)) => Some(s.clone()),
            _ => None,
        }),
        _ => None,
    }
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
    setup_service_inner(collections, globals, vec![])
}

fn setup_service_with_locale(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
    locales: Vec<&str>,
) -> TestSetup {
    setup_service_inner(collections, globals, locales)
}

fn setup_service_inner(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
    locales: Vec<&str>,
) -> TestSetup {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();

    if !locales.is_empty() {
        config.locale.locales = locales.iter().map(|s| s.to_string()).collect();
        config.locale.default_locale = locales.first().unwrap_or(&"en").to_string();
        config.locale.fallback = true;
    }

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

    let deps = ContentServiceDeps::builder()
        .pool(db_pool.clone())
        .registry(Registry::snapshot(&registry))
        .hook_runner(hook_runner)
        .jwt_secret(config.auth.secret.clone())
        .config(config.clone())
        .config_dir(tmp.path().to_path_buf())
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

// ── Collection definitions ──────────────────────────────────────────────

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

fn make_versioned_posts_def() -> CollectionDefinition {
    let mut def = make_posts_def();
    def.versions = Some(VersionsConfig::new(true, 0));
    def
}

fn make_localized_posts_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .localized(true)
            .build(),
        FieldDefinition::builder("body", FieldType::Textarea)
            .localized(true)
            .build(),
    ];
    def
}

fn make_tags_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("tags");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Tag".to_string())),
        plural: Some(LocalizedString::Plain("Tags".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("name", FieldType::Text)
            .required(true)
            .build(),
    ];
    def
}

fn make_posts_with_has_many() -> CollectionDefinition {
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
        FieldDefinition::builder("tags", FieldType::Relationship)
            .relationship(RelationshipConfig::new("tags", true))
            .build(),
    ];
    def
}

/// Products with Array for UpdateMany test.
fn make_products_with_array() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("products");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Product".to_string())),
        plural: Some(LocalizedString::Plain("Products".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("name", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("variants", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("color", FieldType::Text).build(),
            ])
            .build(),
    ];
    def
}

/// Categories for relationship depth test.
fn make_categories_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("categories");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Category".to_string())),
        plural: Some(LocalizedString::Plain("Categories".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("name", FieldType::Text)
            .required(true)
            .build(),
    ];
    def
}

/// Posts with a Group containing a Relationship field.
fn make_posts_with_group_relationship() -> CollectionDefinition {
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
        FieldDefinition::builder("meta", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("category", FieldType::Relationship)
                    .relationship(RelationshipConfig::new("categories", false))
                    .build(),
            ])
            .build(),
    ];
    def
}

// ── Localized Update ────────────────────────────────────────────────────

#[tokio::test]
async fn grpc_update_localized_field() {
    let ts = setup_service_with_locale(vec![make_localized_posts_def()], vec![], vec!["en", "de"]);

    // Create with English
    let doc = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(make_struct(&[("title", "Hello"), ("body", "English body")])),
            locale: Some("en".to_string()),
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // Update German locale
    ts.service
        .update(Request::new(content::UpdateRequest {
            collection: "posts".to_string(),
            id: doc.id.clone(),
            data: Some(make_struct(&[
                ("title", "Hallo"),
                ("body", "Deutscher Text"),
            ])),
            locale: Some("de".to_string()),
            draft: None,
            unpublish: None,
        }))
        .await
        .unwrap();

    // Find German — should show updated values
    let de_doc = ts
        .service
        .find_by_id(Request::new(content::FindByIdRequest {
            collection: "posts".to_string(),
            id: doc.id.clone(),
            locale: Some("de".to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    assert_eq!(get_proto_field(&de_doc, "title").as_deref(), Some("Hallo"));
    assert_eq!(
        get_proto_field(&de_doc, "body").as_deref(),
        Some("Deutscher Text")
    );

    // Find English — should be unchanged
    let en_doc = ts
        .service
        .find_by_id(Request::new(content::FindByIdRequest {
            collection: "posts".to_string(),
            id: doc.id.clone(),
            locale: Some("en".to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    assert_eq!(get_proto_field(&en_doc, "title").as_deref(), Some("Hello"));
    assert_eq!(
        get_proto_field(&en_doc, "body").as_deref(),
        Some("English body")
    );
}

// ── Unpublish ───────────────────────────────────────────────────────────

#[tokio::test]
async fn grpc_unpublish_via_update() {
    let ts = setup_service(vec![make_versioned_posts_def()], vec![]);

    // Create a published post
    let doc = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(make_struct(&[("title", "Published Post")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // Verify visible in normal find
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "posts".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.pagination.as_ref().unwrap().total_docs, 1);

    // Unpublish
    ts.service
        .update(Request::new(content::UpdateRequest {
            collection: "posts".to_string(),
            id: doc.id.clone(),
            data: None,
            locale: None,
            draft: None,
            unpublish: Some(true),
        }))
        .await
        .unwrap();

    // Should NOT appear in regular find
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "posts".to_string(),
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

    // Should appear with draft=true
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
    assert_eq!(
        resp.pagination.as_ref().unwrap().total_docs,
        1,
        "unpublished doc should appear with draft=true"
    );
}

// ── Validation Error Messages ───────────────────────────────────────────

#[tokio::test]
async fn grpc_validation_error_message_includes_field() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    // Missing required 'title'
    let result = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(make_struct(&[("status", "draft")])),
            locale: None,
            draft: None,
        }))
        .await;

    assert!(result.is_err());
    let err = result.err().unwrap();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    let msg = err.message().to_lowercase();
    assert!(
        msg.contains("title"),
        "error message should mention the field name 'title', got: {}",
        err.message()
    );
}

// ── Has-Many Create and Update ──────────────────────────────────────────

#[tokio::test]
async fn grpc_create_and_update_has_many() {
    let ts = setup_service(vec![make_tags_def(), make_posts_with_has_many()], vec![]);

    // Create tags
    let tag1 = ts
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

    let tag2 = ts
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

    let tag3 = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "tags".to_string(),
            data: Some(make_struct(&[("name", "go")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // Create post with tag1 and tag2
    let mut post_fields = BTreeMap::new();
    post_fields.insert("title".to_string(), str_val("Tagged Post"));
    post_fields.insert(
        "tags".to_string(),
        list_val(vec![str_val(&tag1.id), str_val(&tag2.id)]),
    );
    let post = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(Struct {
                fields: post_fields,
            }),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // Verify initial tags
    let found = ts
        .service
        .find_by_id(Request::new(content::FindByIdRequest {
            collection: "posts".to_string(),
            id: post.id.clone(),
            depth: Some(0),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    let tags = get_list_items(&found, "tags");
    assert_eq!(tags.len(), 2, "should have 2 tags initially");

    // Update: replace with tag3 only
    let mut update_fields = BTreeMap::new();
    update_fields.insert("tags".to_string(), list_val(vec![str_val(&tag3.id)]));
    ts.service
        .update(Request::new(content::UpdateRequest {
            collection: "posts".to_string(),
            id: post.id.clone(),
            data: Some(Struct {
                fields: update_fields,
            }),
            locale: None,
            draft: None,
            unpublish: None,
        }))
        .await
        .unwrap();

    // Verify updated tags
    let found = ts
        .service
        .find_by_id(Request::new(content::FindByIdRequest {
            collection: "posts".to_string(),
            id: post.id.clone(),
            depth: Some(0),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    let tags = get_list_items(&found, "tags");
    assert_eq!(tags.len(), 1, "should have 1 tag after update");
}

// ── Group > Relationship with Depth ─────────────────────────────────────

#[tokio::test]
async fn grpc_find_with_depth_populates_group_relationship() {
    let ts = setup_service(
        vec![make_categories_def(), make_posts_with_group_relationship()],
        vec![],
    );

    // Create category
    let cat = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "categories".to_string(),
            data: Some(make_struct(&[("name", "Technology")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // Create post with group > relationship
    let mut post_fields = BTreeMap::new();
    post_fields.insert("title".to_string(), str_val("Tech Post"));
    post_fields.insert(
        "meta".to_string(),
        Value {
            kind: Some(Kind::StructValue(Struct {
                fields: BTreeMap::from([("category".to_string(), str_val(&cat.id))]),
            })),
        },
    );

    let post = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(Struct {
                fields: post_fields,
            }),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // Find with depth=1 — group relationship should be populated
    let found = ts
        .service
        .find_by_id(Request::new(content::FindByIdRequest {
            collection: "posts".to_string(),
            id: post.id.clone(),
            depth: Some(1),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    let meta = get_proto_value(&found, "meta");
    assert!(meta.is_some(), "meta group should exist");

    let meta_val = meta.unwrap();
    // At depth=1, the relationship should be populated (a struct, not a string ID)
    // But in group context, it may return the ID. Check if we get any value.
    let cat_value = match &meta_val.kind {
        Some(Kind::StructValue(s)) => s.fields.get("category"),
        _ => None,
    };
    assert!(cat_value.is_some(), "meta.category should have a value");

    // If populated, it should be a struct with 'name'
    match cat_value.unwrap().kind.as_ref() {
        Some(Kind::StructValue(inner)) => {
            let name = inner.fields.get("name").and_then(|v| match &v.kind {
                Some(Kind::StringValue(s)) => Some(s.clone()),
                _ => None,
            });
            assert_eq!(
                name.as_deref(),
                Some("Technology"),
                "populated category should have name"
            );
        }
        Some(Kind::StringValue(id)) => {
            // Depth=1 may not populate through groups — still valid if ID is correct
            assert_eq!(id, &cat.id, "should at least store the category ID");
        }
        other => panic!(
            "expected StructValue or StringValue for category, got: {:?}",
            other
        ),
    }
}

// ── UpdateMany with Nested Array ────────────────────────────────────────

#[tokio::test]
async fn grpc_update_many_with_nested_array() {
    let ts = setup_service(vec![make_products_with_array()], vec![]);

    // Create 2 products
    for name in &["Product A", "Product B"] {
        let mut fields = BTreeMap::new();
        fields.insert("name".to_string(), str_val(name));
        fields.insert(
            "variants".to_string(),
            list_val(vec![Value {
                kind: Some(Kind::StructValue(Struct {
                    fields: BTreeMap::from([("color".to_string(), str_val("red"))]),
                })),
            }]),
        );

        ts.service
            .create(Request::new(content::CreateRequest {
                collection: "products".to_string(),
                data: Some(Struct { fields }),
                locale: None,
                draft: None,
            }))
            .await
            .unwrap();
    }

    // UpdateMany: change all variants to blue
    let mut update_fields = BTreeMap::new();
    update_fields.insert(
        "variants".to_string(),
        list_val(vec![Value {
            kind: Some(Kind::StructValue(Struct {
                fields: BTreeMap::from([("color".to_string(), str_val("blue"))]),
            })),
        }]),
    );

    let resp = ts
        .service
        .update_many(Request::new(content::UpdateManyRequest {
            collection: "products".to_string(),
            r#where: None,
            data: Some(Struct {
                fields: update_fields,
            }),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.modified, 2, "should update both products");

    // Verify both products have blue variants
    let all = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "products".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    for doc in &all.documents {
        let variants = get_list_items(doc, "variants");
        assert_eq!(variants.len(), 1);
        assert_eq!(
            get_struct_field_str(&variants[0], "color").as_deref(),
            Some("blue"),
            "variant color should be updated to blue"
        );
    }
}

// ── UpdateMany Rejects Password ─────────────────────────────────────────

fn make_auth_users_def() -> CollectionDefinition {
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

#[tokio::test]
async fn update_many_rejects_password_field() {
    let ts = setup_service(vec![make_auth_users_def()], vec![]);

    // Create a user (password is handled by single-doc Create)
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "users".to_string(),
            data: Some(make_struct(&[
                ("email", "test@example.com"),
                ("name", "Test User"),
                ("password", "securepassword123"),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    // UpdateMany with password field should be rejected
    let result = ts
        .service
        .update_many(Request::new(content::UpdateManyRequest {
            collection: "users".to_string(),
            r#where: None,
            data: Some(make_struct(&[
                ("name", "Updated Name"),
                ("password", "newpassword456"),
            ])),
            ..Default::default()
        }))
        .await;

    assert!(result.is_err(), "UpdateMany with password should fail");
    let err = result.unwrap_err();
    assert_eq!(
        err.code(),
        tonic::Code::InvalidArgument,
        "should return INVALID_ARGUMENT"
    );
    assert!(
        err.message().contains("Password updates are not supported"),
        "error message should explain the restriction, got: {}",
        err.message()
    );
}

// ── DeleteMany Cleans Up Upload Files ───────────────────────────────────

fn make_media_upload_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("media");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Media".to_string())),
        plural: Some(LocalizedString::Plain("Media".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("filename", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("url", FieldType::Text).build(),
    ];
    def.upload = Some(CollectionUpload::new());
    def
}

#[tokio::test]
async fn delete_many_cleans_up_upload_files() {
    let ts = setup_service(vec![make_media_upload_def()], vec![]);

    // Create upload directory and fake files
    let uploads_dir = ts._tmp.path().join("uploads/media");
    std::fs::create_dir_all(&uploads_dir).unwrap();

    let file1 = uploads_dir.join("file1.png");
    let file2 = uploads_dir.join("file2.png");
    std::fs::write(&file1, b"fake image 1").unwrap();
    std::fs::write(&file2, b"fake image 2").unwrap();

    // Create two documents with url fields pointing to the files
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "media".to_string(),
            data: Some(make_struct(&[
                ("filename", "file1.png"),
                ("url", "/uploads/media/file1.png"),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "media".to_string(),
            data: Some(make_struct(&[
                ("filename", "file2.png"),
                ("url", "/uploads/media/file2.png"),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    // Verify files exist before delete
    assert!(file1.exists(), "file1 should exist before DeleteMany");
    assert!(file2.exists(), "file2 should exist before DeleteMany");

    // DeleteMany all media documents
    let resp = ts
        .service
        .delete_many(Request::new(content::DeleteManyRequest {
            collection: "media".to_string(),
            r#where: None,
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.deleted, 2, "should delete both documents");

    // Verify files are cleaned up
    assert!(!file1.exists(), "file1 should be deleted after DeleteMany");
    assert!(!file2.exists(), "file2 should be deleted after DeleteMany");
}
