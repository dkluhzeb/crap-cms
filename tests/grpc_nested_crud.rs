//! gRPC integration tests for Group + Array + Blocks nested CRUD.
//!
//! Covers creating, reading, and updating documents with nested join data
//! (arrays, blocks, groups) through the gRPC API surface.

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

// ── Helpers ───────────────────────────────────────────────────────────────

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

/// Extract a nested Value from a proto Document's fields struct.
fn get_proto_value(doc: &content::Document, field: &str) -> Option<Value> {
    doc.fields
        .as_ref()
        .and_then(|s| s.fields.get(field).cloned())
}

/// Extract the list of Values from a ListValue field.
fn get_list_items(doc: &content::Document, field: &str) -> Vec<Value> {
    match get_proto_value(doc, field) {
        Some(Value {
            kind: Some(Kind::ListValue(lv)),
        }) => lv.values,
        _ => vec![],
    }
}

/// Extract a string from a nested struct value.
fn get_struct_field_str(val: &Value, field: &str) -> Option<String> {
    match &val.kind {
        Some(Kind::StructValue(s)) => s.fields.get(field).and_then(|v| match &v.kind {
            Some(Kind::StringValue(s)) => Some(s.clone()),
            _ => None,
        }),
        _ => None,
    }
}

/// Extract a nested struct Value from a struct value.
fn get_struct_field_value(val: &Value, field: &str) -> Option<Value> {
    match &val.kind {
        Some(Kind::StructValue(s)) => s.fields.get(field).cloned(),
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
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::test_default();
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

fn setup_service_with_locale(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
    locales: Vec<&str>,
) -> TestSetup {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
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

// ── Collection definitions ──────────────────────────────────────────────

fn make_products_def() -> CollectionDefinition {
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
        FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("meta_title", FieldType::Text).build(),
            ])
            .build(),
        FieldDefinition::builder("variants", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("color", FieldType::Text).build(),
                FieldDefinition::builder("dimensions", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("width", FieldType::Text).build(),
                        FieldDefinition::builder("height", FieldType::Text).build(),
                    ])
                    .build(),
            ])
            .build(),
        FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![
                BlockDefinition::new(
                    "text",
                    vec![FieldDefinition::builder("body", FieldType::Text).build()],
                ),
                BlockDefinition::new(
                    "section",
                    vec![
                        FieldDefinition::builder("heading", FieldType::Text).build(),
                        FieldDefinition::builder("meta", FieldType::Group)
                            .fields(vec![
                                FieldDefinition::builder("author", FieldType::Text).build(),
                            ])
                            .build(),
                    ],
                ),
            ])
            .build(),
    ];
    def
}

/// Products with a required group sub-field for validation tests.
fn make_products_with_required_group_field() -> CollectionDefinition {
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
        FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("meta_title", FieldType::Text)
                    .required(true)
                    .build(),
            ])
            .build(),
    ];
    def
}

/// Products with localized array fields.
fn make_localized_products_def() -> CollectionDefinition {
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
            .localized(true)
            .fields(vec![
                FieldDefinition::builder("color", FieldType::Text).build(),
            ])
            .build(),
    ];
    def
}

/// Build the full product data proto Struct.
fn make_product_data(
    name: &str,
    seo_title: &str,
    variants: Vec<Value>,
    content_blocks: Vec<Value>,
) -> Struct {
    let mut fields = BTreeMap::new();
    fields.insert("name".to_string(), str_val(name));
    fields.insert(
        "seo".to_string(),
        struct_val(&[("meta_title", str_val(seo_title))]),
    );
    fields.insert("variants".to_string(), list_val(variants));
    fields.insert("content".to_string(), list_val(content_blocks));
    Struct { fields }
}

/// Build a single variant value.
fn make_variant(color: &str, width: &str, height: &str) -> Value {
    struct_val(&[
        ("color", str_val(color)),
        (
            "dimensions",
            struct_val(&[("width", str_val(width)), ("height", str_val(height))]),
        ),
    ])
}

/// Build a text block value.
fn make_text_block(body: &str) -> Value {
    struct_val(&[("_block_type", str_val("text")), ("body", str_val(body))])
}

/// Build a section block value.
fn make_section_block(heading: &str, author: &str) -> Value {
    struct_val(&[
        ("_block_type", str_val("section")),
        ("heading", str_val(heading)),
        ("meta", struct_val(&[("author", str_val(author))])),
    ])
}

/// Create a product and return the document.
async fn create_product(ts: &TestSetup, data: Struct) -> content::Document {
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "products".to_string(),
            data: Some(data),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap()
}

/// Find all products.
async fn find_products(ts: &TestSetup) -> Vec<content::Document> {
    ts.service
        .find(Request::new(content::FindRequest {
            collection: "products".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .documents
}

/// Find a single product by ID.
async fn find_product_by_id(ts: &TestSetup, id: &str) -> content::Document {
    ts.service
        .find_by_id(Request::new(content::FindByIdRequest {
            collection: "products".to_string(),
            id: id.to_string(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap()
}

// ── Tests ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn grpc_create_product_with_nested_data() {
    let ts = setup_service(vec![make_products_def()], vec![]);

    let data = make_product_data(
        "Widget",
        "Buy Widget",
        vec![make_variant("red", "10", "20")],
        vec![make_text_block("Hello world")],
    );

    let doc = create_product(&ts, data).await;
    assert!(!doc.id.is_empty(), "created doc should have an id");
    assert_eq!(get_proto_field(&doc, "name").as_deref(), Some("Widget"));
}

#[tokio::test]
async fn grpc_find_product_hydrates_group() {
    let ts = setup_service(vec![make_products_def()], vec![]);

    let data = make_product_data(
        "Widget",
        "SEO Widget",
        vec![make_variant("blue", "5", "10")],
        vec![make_text_block("body")],
    );
    create_product(&ts, data).await;

    let docs = find_products(&ts).await;
    assert_eq!(docs.len(), 1);

    let seo = get_proto_value(&docs[0], "seo");
    assert!(seo.is_some(), "seo group should be present");
    assert_eq!(
        get_struct_field_str(seo.as_ref().unwrap(), "meta_title").as_deref(),
        Some("SEO Widget"),
        "group sub-field meta_title should be hydrated"
    );
}

#[tokio::test]
async fn grpc_find_product_hydrates_array() {
    let ts = setup_service(vec![make_products_def()], vec![]);

    let data = make_product_data(
        "Gadget",
        "SEO Gadget",
        vec![
            make_variant("red", "10", "20"),
            make_variant("blue", "30", "40"),
        ],
        vec![make_text_block("content")],
    );
    let doc = create_product(&ts, data).await;

    let found = find_product_by_id(&ts, &doc.id).await;
    let variants = get_list_items(&found, "variants");
    assert_eq!(variants.len(), 2, "should have 2 variants");

    assert_eq!(
        get_struct_field_str(&variants[0], "color").as_deref(),
        Some("red")
    );
    assert_eq!(
        get_struct_field_str(&variants[1], "color").as_deref(),
        Some("blue")
    );

    // Verify nested group in array
    let dims = get_struct_field_value(&variants[0], "dimensions");
    assert!(dims.is_some(), "variant should have dimensions group");
    assert_eq!(
        get_struct_field_str(dims.as_ref().unwrap(), "width").as_deref(),
        Some("10")
    );
}

#[tokio::test]
async fn grpc_find_product_hydrates_blocks() {
    let ts = setup_service(vec![make_products_def()], vec![]);

    let data = make_product_data(
        "Gizmo",
        "SEO Gizmo",
        vec![make_variant("green", "1", "2")],
        vec![
            make_text_block("Hello"),
            make_section_block("Intro", "Alice"),
        ],
    );
    let doc = create_product(&ts, data).await;

    let found = find_product_by_id(&ts, &doc.id).await;
    let blocks = get_list_items(&found, "content");
    assert_eq!(blocks.len(), 2, "should have 2 content blocks");

    // First block: text
    assert_eq!(
        get_struct_field_str(&blocks[0], "_block_type").as_deref(),
        Some("text")
    );
    assert_eq!(
        get_struct_field_str(&blocks[0], "body").as_deref(),
        Some("Hello")
    );

    // Second block: section with group
    assert_eq!(
        get_struct_field_str(&blocks[1], "_block_type").as_deref(),
        Some("section")
    );
    assert_eq!(
        get_struct_field_str(&blocks[1], "heading").as_deref(),
        Some("Intro")
    );
    let meta = get_struct_field_value(&blocks[1], "meta");
    assert!(meta.is_some(), "section block should have meta group");
    assert_eq!(
        get_struct_field_str(meta.as_ref().unwrap(), "author").as_deref(),
        Some("Alice")
    );
}

#[tokio::test]
async fn grpc_find_by_id_returns_all_nested() {
    let ts = setup_service(vec![make_products_def()], vec![]);

    let data = make_product_data(
        "AllNested",
        "All SEO",
        vec![make_variant("purple", "7", "8")],
        vec![make_section_block("Section", "Bob")],
    );
    let doc = create_product(&ts, data).await;

    let found = find_product_by_id(&ts, &doc.id).await;

    // Group
    let seo = get_proto_value(&found, "seo");
    assert_eq!(
        get_struct_field_str(seo.as_ref().unwrap(), "meta_title").as_deref(),
        Some("All SEO")
    );

    // Array
    let variants = get_list_items(&found, "variants");
    assert_eq!(variants.len(), 1);
    assert_eq!(
        get_struct_field_str(&variants[0], "color").as_deref(),
        Some("purple")
    );

    // Blocks
    let blocks = get_list_items(&found, "content");
    assert_eq!(blocks.len(), 1);
    assert_eq!(
        get_struct_field_str(&blocks[0], "_block_type").as_deref(),
        Some("section")
    );
}

#[tokio::test]
async fn grpc_update_replaces_array_rows() {
    let ts = setup_service(vec![make_products_def()], vec![]);

    let data = make_product_data(
        "Updatable",
        "SEO",
        vec![
            make_variant("red", "1", "2"),
            make_variant("blue", "3", "4"),
        ],
        vec![make_text_block("body")],
    );
    let doc = create_product(&ts, data).await;

    // Verify initial state
    let found = find_product_by_id(&ts, &doc.id).await;
    assert_eq!(get_list_items(&found, "variants").len(), 2);

    // Update: replace with single variant
    let mut update_fields = BTreeMap::new();
    update_fields.insert(
        "variants".to_string(),
        list_val(vec![make_variant("green", "5", "6")]),
    );
    ts.service
        .update(Request::new(content::UpdateRequest {
            collection: "products".to_string(),
            id: doc.id.clone(),
            data: Some(Struct {
                fields: update_fields,
            }),
            locale: None,
            draft: None,
            unpublish: None,
        }))
        .await
        .unwrap();

    // Verify update
    let found = find_product_by_id(&ts, &doc.id).await;
    let variants = get_list_items(&found, "variants");
    assert_eq!(variants.len(), 1, "should have 1 variant after update");
    assert_eq!(
        get_struct_field_str(&variants[0], "color").as_deref(),
        Some("green")
    );
}

#[tokio::test]
async fn grpc_update_replaces_blocks() {
    let ts = setup_service(vec![make_products_def()], vec![]);

    let data = make_product_data(
        "BlockUpdate",
        "SEO",
        vec![make_variant("red", "1", "2")],
        vec![make_text_block("original")],
    );
    let doc = create_product(&ts, data).await;

    // Verify text block exists
    let found = find_product_by_id(&ts, &doc.id).await;
    let blocks = get_list_items(&found, "content");
    assert_eq!(
        get_struct_field_str(&blocks[0], "_block_type").as_deref(),
        Some("text")
    );

    // Update: replace with section block
    let mut update_fields = BTreeMap::new();
    update_fields.insert(
        "content".to_string(),
        list_val(vec![make_section_block("New Section", "Charlie")]),
    );
    ts.service
        .update(Request::new(content::UpdateRequest {
            collection: "products".to_string(),
            id: doc.id.clone(),
            data: Some(Struct {
                fields: update_fields,
            }),
            locale: None,
            draft: None,
            unpublish: None,
        }))
        .await
        .unwrap();

    let found = find_product_by_id(&ts, &doc.id).await;
    let blocks = get_list_items(&found, "content");
    assert_eq!(blocks.len(), 1);
    assert_eq!(
        get_struct_field_str(&blocks[0], "_block_type").as_deref(),
        Some("section"),
        "block type should be updated to section"
    );
    assert_eq!(
        get_struct_field_str(&blocks[0], "heading").as_deref(),
        Some("New Section")
    );
}

#[tokio::test]
async fn grpc_update_group_subfield() {
    let ts = setup_service(vec![make_products_def()], vec![]);

    let data = make_product_data(
        "GroupUpdate",
        "Original SEO",
        vec![make_variant("red", "1", "2")],
        vec![make_text_block("body")],
    );
    let doc = create_product(&ts, data).await;

    // Update group sub-field
    let mut update_fields = BTreeMap::new();
    update_fields.insert(
        "seo".to_string(),
        struct_val(&[("meta_title", str_val("Updated SEO"))]),
    );
    ts.service
        .update(Request::new(content::UpdateRequest {
            collection: "products".to_string(),
            id: doc.id.clone(),
            data: Some(Struct {
                fields: update_fields,
            }),
            locale: None,
            draft: None,
            unpublish: None,
        }))
        .await
        .unwrap();

    let found = find_product_by_id(&ts, &doc.id).await;
    let seo = get_proto_value(&found, "seo").unwrap();
    assert_eq!(
        get_struct_field_str(&seo, "meta_title").as_deref(),
        Some("Updated SEO"),
        "group sub-field should be updated"
    );
}

#[tokio::test]
async fn grpc_validation_required_in_group() {
    let ts = setup_service(vec![make_products_with_required_group_field()], vec![]);

    // Create with missing required group sub-field
    let result = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "products".to_string(),
            data: Some(make_struct(&[("name", "NoSEO")])),
            locale: None,
            draft: None,
        }))
        .await;

    assert!(
        result.is_err(),
        "should fail when required group sub-field is missing"
    );
    let err = result.err().unwrap();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn grpc_create_multiple_array_rows() {
    let ts = setup_service(vec![make_products_def()], vec![]);

    let data = make_product_data(
        "MultiVariant",
        "SEO",
        vec![
            make_variant("red", "1", "2"),
            make_variant("blue", "3", "4"),
            make_variant("green", "5", "6"),
        ],
        vec![make_text_block("body")],
    );
    let doc = create_product(&ts, data).await;

    let found = find_product_by_id(&ts, &doc.id).await;
    let variants = get_list_items(&found, "variants");
    assert_eq!(variants.len(), 3, "should have 3 variants");

    let colors: Vec<Option<String>> = variants
        .iter()
        .map(|v| get_struct_field_str(v, "color"))
        .collect();
    assert_eq!(
        colors,
        vec![
            Some("red".to_string()),
            Some("blue".to_string()),
            Some("green".to_string()),
        ]
    );
}

#[tokio::test]
async fn grpc_create_multiple_block_types() {
    let ts = setup_service(vec![make_products_def()], vec![]);

    let data = make_product_data(
        "MultiBlock",
        "SEO",
        vec![make_variant("red", "1", "2")],
        vec![
            make_text_block("First paragraph"),
            make_section_block("Section One", "Alice"),
            make_text_block("Second paragraph"),
        ],
    );
    let doc = create_product(&ts, data).await;

    let found = find_product_by_id(&ts, &doc.id).await;
    let blocks = get_list_items(&found, "content");
    assert_eq!(blocks.len(), 3, "should have 3 blocks");

    let types: Vec<Option<String>> = blocks
        .iter()
        .map(|b| get_struct_field_str(b, "_block_type"))
        .collect();
    assert_eq!(
        types,
        vec![
            Some("text".to_string()),
            Some("section".to_string()),
            Some("text".to_string()),
        ],
        "block types should preserve order"
    );
}

#[tokio::test]
async fn grpc_localized_array_crud() {
    let ts = setup_service_with_locale(
        vec![make_localized_products_def()],
        vec![],
        vec!["en", "de"],
    );

    // Create with English locale and array data
    let mut fields = BTreeMap::new();
    fields.insert("name".to_string(), str_val("Localized Widget"));
    fields.insert(
        "variants".to_string(),
        list_val(vec![struct_val(&[("color", str_val("red"))])]),
    );

    let doc = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "products".to_string(),
            data: Some(Struct { fields }),
            locale: Some("en".to_string()),
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // Find with English locale — should return array data
    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "products".to_string(),
            locale: Some("en".to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.documents.len(), 1);
    let variants = get_list_items(&resp.documents[0], "variants");
    assert_eq!(variants.len(), 1, "English locale should have 1 variant");
    assert_eq!(
        get_struct_field_str(&variants[0], "color").as_deref(),
        Some("red")
    );

    // Update with German locale — different array data
    let mut de_fields = BTreeMap::new();
    de_fields.insert(
        "variants".to_string(),
        list_val(vec![struct_val(&[("color", str_val("rot"))])]),
    );
    ts.service
        .update(Request::new(content::UpdateRequest {
            collection: "products".to_string(),
            id: doc.id.clone(),
            data: Some(Struct { fields: de_fields }),
            locale: Some("de".to_string()),
            draft: None,
            unpublish: None,
        }))
        .await
        .unwrap();

    // Verify German locale returns German data
    let found = ts
        .service
        .find_by_id(Request::new(content::FindByIdRequest {
            collection: "products".to_string(),
            id: doc.id.clone(),
            locale: Some("de".to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    let de_variants = get_list_items(&found, "variants");
    assert_eq!(de_variants.len(), 1);
    assert_eq!(
        get_struct_field_str(&de_variants[0], "color").as_deref(),
        Some("rot"),
        "German locale should return German array data"
    );

    // Verify English locale is unchanged
    let found_en = ts
        .service
        .find_by_id(Request::new(content::FindByIdRequest {
            collection: "products".to_string(),
            id: doc.id.clone(),
            locale: Some("en".to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    let en_variants = get_list_items(&found_en, "variants");
    assert_eq!(
        get_struct_field_str(&en_variants[0], "color").as_deref(),
        Some("red"),
        "English locale should be unchanged"
    );
}
