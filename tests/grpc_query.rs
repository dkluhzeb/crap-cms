//! Query-related gRPC integration tests: depth/relationships, dot-notation
//! filters, filter operators, unique constraints, custom validators,
//! field-level hooks, and collection-level hooks.
//!
//! Uses ContentService directly (no network) via ContentApi trait.

use std::collections::BTreeMap;
use std::sync::Arc;

use prost_types::{value::Kind, ListValue, Struct, Value};
use tonic::Request;

use crap_cms::api::content;
use crap_cms::api::content::content_api_server::ContentApi;
use crap_cms::api::service::ContentService;
use crap_cms::config::*;
use crap_cms::core::collection::*;
use crap_cms::core::email::EmailRenderer;
use crap_cms::core::field::*;
use crap_cms::core::Registry;
use crap_cms::db::{migrate, pool};
use crap_cms::hooks::lifecycle::HookRunner;

// ── Helpers ───────────────────────────────────────────────────────────────

#[allow(dead_code)]
fn make_posts_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = CollectionLabels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
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
    ];
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
        HookRunner::builder()
            .config_dir(tmp.path())
            .registry(registry.clone())
            .config(&config)
            .build()
            .expect("create hook runner");

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

/// Helper to build a setup with a custom init.lua for hook tests.
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
        HookRunner::builder()
            .config_dir(tmp.path())
            .registry(registry.clone())
            .config(&config)
            .build()
            .expect("create hook runner");

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

// ── Dot-notation helpers ──────────────────────────────────────────────────

fn str_val(s: &str) -> Value {
    Value { kind: Some(Kind::StringValue(s.to_string())) }
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

fn make_product_struct(
    name: &str,
    seo_title: &str,
    color: &str,
    width: &str,
    height: &str,
    content_blocks: Vec<Value>,
) -> Struct {
    let mut fields = BTreeMap::new();
    fields.insert("name".to_string(), str_val(name));
    fields.insert(
        "seo".to_string(),
        struct_val(&[("meta_title", str_val(seo_title))]),
    );
    fields.insert(
        "variants".to_string(),
        list_val(vec![struct_val(&[
            ("color", str_val(color)),
            (
                "dimensions",
                struct_val(&[("width", str_val(width)), ("height", str_val(height))]),
            ),
        ])]),
    );
    fields.insert("content".to_string(), list_val(content_blocks));
    Struct { fields }
}

fn make_products_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("products");
    def.labels = CollectionLabels {
        singular: Some(LocalizedString::Plain("Product".to_string())),
        plural: Some(LocalizedString::Plain("Products".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
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
                BlockDefinition::new("text", vec![FieldDefinition {
                    name: "body".to_string(),
                    ..Default::default()
                }]),
                BlockDefinition::new("section", vec![
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
                ]),
            ],
            ..Default::default()
        },
    ];
    def
}

fn make_categories_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("categories");
    def.labels = CollectionLabels {
        singular: Some(LocalizedString::Plain("Category".to_string())),
        plural: Some(LocalizedString::Plain("Categories".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![FieldDefinition {
        name: "name".to_string(),
        required: true,
        ..Default::default()
    }];
    def
}

fn make_posts_with_relationship() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = CollectionLabels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition {
            name: "title".to_string(),
            required: true,
            ..Default::default()
        },
        FieldDefinition {
            name: "category".to_string(),
            field_type: FieldType::Relationship,
            relationship: Some(RelationshipConfig::new("categories", false)),
            ..Default::default()
        },
    ];
    def
}

// ── Depth > 0 in gRPC ────────────────────────────────────────────────────

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

    // Create a post with the category relationship
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(make_struct(&[
                ("title", "Rust Post"),
                ("category", &cat_doc.id),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    // Find with depth=1 — category should be populated
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
    let post = &resp.documents[0];
    let fields = post.fields.as_ref().unwrap();
    let cat_field = fields.fields.get("category");
    assert!(cat_field.is_some(), "category field should be present");

    match &cat_field.unwrap().kind {
        Some(Kind::StructValue(s)) => {
            assert!(
                s.fields.contains_key("name"),
                "Populated category should have 'name' field"
            );
        }
        other => {
            panic!(
                "Expected a StructValue (populated document) at depth=1, got: {:?}",
                other
            );
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

    // Create a post linked to the category
    let post_doc = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(make_struct(&[
                ("title", "Science Post"),
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

    // FindByID with default depth (1) — category should be populated
    let found = ts
        .service
        .find_by_id(Request::new(content::FindByIdRequest {
            collection: "posts".to_string(),
            id: post_doc.id.clone(),
            depth: Some(1),
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
        Some(Kind::StructValue(s)) => {
            assert!(
                s.fields.contains_key("name"),
                "Populated category should have 'name' field"
            );
        }
        other => {
            panic!(
                "Expected a StructValue (populated document) at depth=1, got: {:?}",
                other
            );
        }
    }
}

// ── Dot-notation filter tests ─────────────────────────────────────────────

#[tokio::test]
async fn find_with_where_dot_notation() {
    let ts = setup_service(vec![make_products_def()], vec![]);

    // Create Widget (text block, red variant)
    let widget_data = make_product_struct(
        "Widget",
        "Widget SEO",
        "red",
        "10",
        "20",
        vec![struct_val(&[
            ("_block_type", str_val("text")),
            ("body", str_val("Widget description")),
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

    // Create Gadget (section block, blue variant)
    let gadget_data = make_product_struct(
        "Gadget",
        "Gadget SEO",
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

