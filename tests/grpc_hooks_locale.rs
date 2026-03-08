//! Localization, drafts, versions, complex globals, has-many relationships,
//! bulk operations, count, FTS search, and jobs RPC tests.
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

fn make_localized_posts_def() -> CollectionDefinition {
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
            localized: true,
            ..Default::default()
        },
        FieldDefinition {
            name: "body".to_string(),
            field_type: FieldType::Textarea,
            ..Default::default()
        },
    ];
    def
}

fn make_versioned_posts_def() -> CollectionDefinition {
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
            name: "body".to_string(),
            field_type: FieldType::Textarea,
            ..Default::default()
        },
    ];
    def.versions = Some(VersionsConfig::new(true, 10));
    def
}

fn make_tags_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("tags");
    def.labels = CollectionLabels {
        singular: Some(LocalizedString::Plain("Tag".to_string())),
        plural: Some(LocalizedString::Plain("Tags".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![FieldDefinition {
        name: "name".to_string(),
        required: true,
        ..Default::default()
    }];
    def
}

fn make_posts_with_has_many() -> CollectionDefinition {
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
            name: "tags".to_string(),
            field_type: FieldType::Relationship,
            relationship: Some(RelationshipConfig::new("tags", true)),
            ..Default::default()
        },
    ];
    def
}

fn make_complex_global_def() -> GlobalDefinition {
    let mut def = GlobalDefinition::new("site_config");
    def.labels = CollectionLabels {
        singular: Some(LocalizedString::Plain("Site Config".to_string())),
        plural: None,
    };
    def.fields = vec![
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
            blocks: vec![BlockDefinition::new("hero", vec![FieldDefinition {
                name: "heading".to_string(),
                ..Default::default()
            }])],
            ..Default::default()
        },
    ];
    def
}

// ── Group 6: Localization (gRPC) ──────────────────────────────────────────

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

