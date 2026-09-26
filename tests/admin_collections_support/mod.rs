//! Shared setup helpers for the `admin_collections*` integration test
//! binaries.
//!
//! `tests/admin_collections_support/` is a subdirectory, so Cargo never
//! compiles it as a test binary of its own; each binary pulls it in with
//! `mod admin_collections_support;` and uses only the helpers its tests need.

#![allow(dead_code)]

use std::{collections::HashMap, fs, sync::Arc};

use axum::body::Body;
use http_body_util::BodyExt;
use serde_json::json;

use crap_cms::{
    admin::{AdminState, server::build_router, templates, translations::Translations},
    config::CrapConfig,
    core::{
        DocumentFields, JwtSecret, LiveSlots, Registry, auth,
        collection::{
            AdminConfig, Auth, CollectionDefinition, GlobalDefinition, Labels, VersionsConfig,
        },
        field::{FieldDefinition, FieldType, LocalizedString},
    },
    db::{migrate, pool, query},
    hooks::lifecycle::HookRunner,
};

pub fn make_posts_def() -> CollectionDefinition {
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
    ];
    def
}

pub fn make_users_def() -> CollectionDefinition {
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
    def.auth = Some(Auth::enabled());
    def
}

pub struct TestApp {
    pub tmp: tempfile::TempDir,
    pub router: axum::Router,
    pub pool: crap_cms::db::DbPool,
    pub registry: Arc<Registry>,
    pub jwt_secret: JwtSecret,
}

pub fn setup_app(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
) -> TestApp {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;
    setup_app_with_config(collections, globals, config)
}

pub fn setup_app_with_config(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
    config: CrapConfig,
) -> TestApp {
    let tmp = tempfile::tempdir().expect("tempdir");

    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");

    let shared = Registry::shared();
    {
        let mut reg = shared.write().unwrap();
        for def in collections {
            reg.register_collection(def);
        }
        for def in globals {
            reg.register_global(def);
        }
    }

    let registry = Registry::snapshot(&shared);
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync schema");

    let hook_runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("create hook runner");

    let translations = Arc::new(Translations::load(tmp.path()));
    let handlebars = templates::create_handlebars(tmp.path(), false, translations.clone(), None)
        .expect("create handlebars");

    let has_auth = registry
        .collections
        .values()
        .any(|d| d.is_auth_collection());

    let storage = crap_cms::core::upload::create_storage(
        tmp.path(),
        &crap_cms::config::UploadConfig::default(),
    )
    .unwrap();
    let token_provider: crap_cms::core::SharedTokenProvider = std::sync::Arc::new(
        crap_cms::core::auth::JwtTokenProvider::new("test-jwt-secret"),
    );
    let infra = crap_cms::admin::test_support::test_infra(
        db_pool.clone(),
        Arc::clone(&registry),
        hook_runner,
        storage,
        token_provider,
        &config,
        tmp.path(),
    );

    let state = AdminState {
        mcp_sessions: Arc::default(),
        infra,
        config,
        config_dir: tmp.path().to_path_buf(),
        handlebars,
        jwt_secret: "test-jwt-secret".into(),
        email_provider: crap_cms::core::email::create_email_provider(
            &crap_cms::config::EmailConfig::default(),
        )
        .unwrap(),
        login_limiter: std::sync::Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
            5, 300,
        )),
        ip_login_limiter: std::sync::Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
            20, 300,
        )),
        forgot_password_limiter: std::sync::Arc::new(
            crap_cms::core::rate_limit::LoginRateLimiter::new(3, 900),
        ),
        ip_forgot_password_limiter: std::sync::Arc::new(
            crap_cms::core::rate_limit::LoginRateLimiter::new(20, 900),
        ),
        mfa_limiter: std::sync::Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(5, 300)),
        ip_mfa_limiter: std::sync::Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
            20, 300,
        )),
        has_auth,
        translations,
        sse_slots: LiveSlots::new(0, 0),
        shutdown: tokio_util::sync::CancellationToken::new(),
        password_provider: std::sync::Arc::new(crap_cms::core::auth::Argon2PasswordProvider),
        subscriber_send_timeout_ms: 1000,
        custom_pages: crap_cms::admin::custom_pages::CustomPageRegistry::default(),
    };

    let router = build_router(state);

    TestApp {
        tmp,
        router,
        pool: db_pool,
        registry,
        jwt_secret: "test-jwt-secret".into(),
    }
}

pub fn create_test_user(app: &TestApp, email: &str, password: &str) -> String {
    let reg = &app.registry;
    let def = reg.get_collection("users").unwrap().clone();

    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields = HashMap::from([
        ("email".to_string(), json!(email)),
        ("name".to_string(), json!("Test User")),
    ])
    .into();
    let doc = query::create(&tx, "users", &def, &data, None).unwrap();
    query::update_password(&tx, "users", &doc.id, password).unwrap();
    tx.commit().unwrap();
    doc.id.to_string()
}

pub fn make_auth_cookie(app: &TestApp, user_id: &str, email: &str) -> String {
    // `update_password` bumps `_session_version` to 1 the moment a password
    // is set; the evaluator rejects a default-built Claims (session_version
    // = 0) as `Invalid(StaleSession)`. Read the user's current version so
    // the test cookie matches the DB.
    let conn = app.pool.get().unwrap();
    let session_version =
        crap_cms::db::query::auth::get_session_version(&conn, "users", user_id).unwrap_or(0);
    drop(conn);
    let claims = auth::Claims::builder(user_id, "users")
        .email(email)
        .session_version(session_version)
        .exp(u64::try_from(chrono::Utc::now().timestamp()).unwrap() + 3600)
        .build()
        .unwrap();
    let token = auth::create_token(&claims, app.jwt_secret.as_ref()).unwrap();
    format!("crap_session={token}")
}

pub const TEST_CSRF: &str = "test-csrf-token-12345";

pub fn auth_and_csrf(auth_cookie: &str) -> String {
    format!("{auth_cookie}; crap_csrf={TEST_CSRF}")
}

pub async fn body_string(body: Body) -> String {
    let bytes = body.collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

pub fn make_versioned_posts_def() -> CollectionDefinition {
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
    def.admin = AdminConfig {
        use_as_title: Some("title".to_string()),
        ..AdminConfig::default()
    };
    def.versions = Some(VersionsConfig::new(true, 10));
    def
}

/// Write `hooks/access.lua` into the app's config dir; hooks load lazily, so
/// rules written after setup are picked up on first use.
pub fn write_access_hooks(config_dir: &std::path::Path, functions: &str) {
    let hooks = config_dir.join("hooks");
    fs::create_dir_all(&hooks).unwrap();
    fs::write(
        hooks.join("access.lua"),
        format!("local M = {{}}\n{functions}\nreturn M\n"),
    )
    .unwrap();
}
