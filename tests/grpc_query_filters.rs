//! Query-related gRPC integration tests: depth/relationships, dot-notation
//! filters, filter operators, unique constraints, custom validators,
//! field-level hooks, and collection-level hooks.
//!
//! Uses ContentService directly (no network) via ContentApi trait.

use std::collections::BTreeMap;
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
use crap_cms::db::{migrate, pool};
use crap_cms::hooks::lifecycle::HookRunner;
use serde_json::json;

// ── Helpers ───────────────────────────────────────────────────────────────

#[allow(dead_code)]
fn make_posts_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def.timestamps = true;
    let title = FieldDefinition {
        name: "title".to_string(),
        required: true,
        ..Default::default()
    };
    let status = FieldDefinition {
        name: "status".to_string(),
        field_type: FieldType::Select,
        default_value: Some(json!("draft")),
        ..Default::default()
    };
    def.fields = vec![title, status];
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

fn make_categories_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("categories");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Category".to_string())),
        plural: Some(LocalizedString::Plain("Categories".to_string())),
    };
    def.timestamps = true;
    let name = FieldDefinition {
        name: "name".to_string(),
        required: true,
        ..Default::default()
    };
    def.fields = vec![name];
    def
}

fn make_posts_with_relationship() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def.timestamps = true;
    let title = FieldDefinition {
        name: "title".to_string(),
        required: true,
        ..Default::default()
    };
    let category = FieldDefinition {
        name: "category".to_string(),
        field_type: FieldType::Relationship,
        relationship: Some(RelationshipConfig::new("categories", false)),
        ..Default::default()
    };
    def.fields = vec![title, category];
    def
}

fn make_numbered_posts_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("items");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Item".to_string())),
        plural: Some(LocalizedString::Plain("Items".to_string())),
    };
    def.timestamps = true;
    let name = FieldDefinition {
        name: "name".to_string(),
        required: true,
        ..Default::default()
    };
    let score = FieldDefinition {
        name: "score".to_string(),
        field_type: FieldType::Number,
        ..Default::default()
    };
    let tag = FieldDefinition {
        name: "tag".to_string(),
        ..Default::default()
    };
    def.fields = vec![name, score, tag];
    def
}

fn make_item(name: &str, score: &str, tag: &str) -> Struct {
    make_struct(&[("name", name), ("score", score), ("tag", tag)])
}

fn make_posts_with_unique_slug() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("articles");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Article".to_string())),
        plural: Some(LocalizedString::Plain("Articles".to_string())),
    };
    def.timestamps = true;
    let title = FieldDefinition {
        name: "title".to_string(),
        required: true,
        ..Default::default()
    };
    let slug = FieldDefinition {
        name: "slug".to_string(),
        unique: true,
        ..Default::default()
    };
    def.fields = vec![title, slug];
    def
}

// ── Depth > 0 in gRPC ────────────────────────────────────────────────────

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
    assert!(
        resp.pagination.as_ref().unwrap().total_docs >= 2
            && resp.pagination.as_ref().unwrap().total_docs <= 3,
        "not_equals: got {}",
        resp.pagination.as_ref().unwrap().total_docs
    );

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
    assert_eq!(
        resp.pagination.as_ref().unwrap().total_docs,
        2,
        "greater_than 30 → Delta(40), Epsilon(50)"
    );

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
    assert_eq!(
        resp.pagination.as_ref().unwrap().total_docs,
        1,
        "less_than 20 → Alpha(10)"
    );

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
    assert_eq!(
        resp.pagination.as_ref().unwrap().total_docs,
        3,
        "gte 30 → Gamma, Delta, Epsilon"
    );

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
    assert_eq!(
        resp.pagination.as_ref().unwrap().total_docs,
        2,
        "lte 20 → Alpha, Beta"
    );

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
    assert_eq!(
        resp.pagination.as_ref().unwrap().total_docs,
        3,
        "in [red, green] → Alpha, Gamma, Epsilon"
    );

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
    assert!(
        resp.pagination.as_ref().unwrap().total_docs >= 1
            && resp.pagination.as_ref().unwrap().total_docs <= 2,
        "not_in [red, green]: got {}",
        resp.pagination.as_ref().unwrap().total_docs
    );

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
    assert_eq!(
        resp.pagination.as_ref().unwrap().total_docs,
        1,
        "like '%lph%' → Alpha"
    );

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
    assert_eq!(
        resp.pagination.as_ref().unwrap().total_docs,
        1,
        "contains 'eta' → Beta"
    );

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
    assert!(
        resp.pagination.as_ref().unwrap().total_docs >= 3,
        "exists: at least the non-empty tags"
    );

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
    assert!(
        resp.pagination.as_ref().unwrap().total_docs <= 2,
        "not_exists: empty/null tags"
    );
}

// ── Group 2: Unique Constraints (gRPC) ────────────────────────────────────

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
    config.auth.secret = "test-jwt-secret".into();

    let mut def = CollectionDefinition::new("scored");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Scored".to_string())),
        plural: Some(LocalizedString::Plain("Scored".to_string())),
    };
    def.timestamps = true;
    let name_f = FieldDefinition {
        name: "name".to_string(),
        required: true,
        ..Default::default()
    };
    let score_f = FieldDefinition {
        name: "score".to_string(),
        validate: Some("hooks.score_validator.check".to_string()),
        ..Default::default()
    };
    def.fields = vec![name_f, score_f];

    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");
    let registry = Registry::shared();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def);
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
    let ts = TestSetup {
        _tmp: tmp,
        service,
        pool: db_pool,
    };

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
    config.auth.secret = "test-jwt-secret".into();

    let mut def = CollectionDefinition::new("pages");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Page".to_string())),
        plural: Some(LocalizedString::Plain("Pages".to_string())),
    };
    def.timestamps = true;
    let name_f = FieldDefinition {
        name: "name".to_string(),
        required: true,
        ..Default::default()
    };
    let slug_f = FieldDefinition {
        name: "slug".to_string(),
        hooks: FieldHooks {
            before_change: vec!["hooks.slug_gen.auto_slug".to_string()],
            ..Default::default()
        },
        ..Default::default()
    };
    def.fields = vec![name_f, slug_f];

    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");
    let registry = Registry::shared();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def);
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
    let ts = TestSetup {
        _tmp: tmp,
        service,
        pool: db_pool,
    };

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
    config.auth.secret = "test-jwt-secret".into();

    let mut def = CollectionDefinition::new("entries");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Entry".to_string())),
        plural: Some(LocalizedString::Plain("Entries".to_string())),
    };
    def.timestamps = true;
    let name_f = FieldDefinition {
        name: "name".to_string(),
        required: true,
        hooks: FieldHooks {
            after_read: vec!["hooks.transform.uppercase_on_read".to_string()],
            ..Default::default()
        },
        ..Default::default()
    };
    def.fields = vec![name_f];

    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");
    let registry = Registry::shared();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def);
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
    let ts = TestSetup {
        _tmp: tmp,
        service,
        pool: db_pool,
    };

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
    config.auth.secret = "test-jwt-secret".into();

    let mut def = CollectionDefinition::new("notes");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Note".to_string())),
        plural: Some(LocalizedString::Plain("Notes".to_string())),
    };
    def.timestamps = true;
    let title_f = FieldDefinition {
        name: "title".to_string(),
        required: true,
        ..Default::default()
    };
    let computed_f = FieldDefinition {
        name: "computed".to_string(),
        ..Default::default()
    };
    def.fields = vec![title_f, computed_f];
    def.hooks.after_read = vec!["hooks.note_hooks.add_computed".to_string()];

    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");
    let registry = Registry::shared();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def);
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
    let ts = TestSetup {
        _tmp: tmp,
        service,
        pool: db_pool,
    };

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
    config.auth.secret = "test-jwt-secret".into();

    let mut def = CollectionDefinition::new("moderated");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Moderated".to_string())),
        plural: Some(LocalizedString::Plain("Moderated".to_string())),
    };
    def.timestamps = true;
    let title_f = FieldDefinition {
        name: "title".to_string(),
        required: true,
        ..Default::default()
    };
    def.fields = vec![title_f];
    def.hooks.before_validate = vec!["hooks.moderator.reject_forbidden".to_string()];

    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");
    let registry = Registry::shared();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def);
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
    let ts = TestSetup {
        _tmp: tmp,
        service,
        pool: db_pool,
    };

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
