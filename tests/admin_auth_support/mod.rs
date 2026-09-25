//! Shared setup helpers for the `admin_auth_*` integration test binaries.
//!
//! `tests/admin_auth_support/` is a subdirectory, so Cargo never compiles it
//! as a test binary of its own; each `admin_auth_*` binary pulls it in with
//! `mod admin_auth_support;` and uses only the helpers its tests need.

#![allow(dead_code)]

use serde_json::json;
use std::{collections::HashMap, sync::Arc};

use axum::body::Body;
use http_body_util::BodyExt;
use tokio_util::sync::CancellationToken;

use crap_cms::{
    admin::{AdminState, server::build_router, templates},
    config::CrapConfig,
    core::{
        DocumentFields, JwtSecret, LiveSlots, Registry, auth,
        collection::{Auth, CollectionDefinition, GlobalDefinition, Labels},
        field::{FieldDefinition, FieldType, LocalizedString},
        rate_limit::LoginRateLimiter,
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
        FieldDefinition::builder("role", FieldType::Text).build(),
    ];
    def.auth = Some(Auth::enabled());
    def
}

pub struct TestApp {
    pub _tmp: tempfile::TempDir,
    pub router: axum::Router,
    pub pool: crap_cms::db::DbPool,
    pub registry: Arc<crap_cms::core::Registry>,
    pub jwt_secret: JwtSecret,
    /// The IP forgot-password limiter the router was built with — exposed so
    /// rate-limit tests can inspect/seed the same `Arc` the handlers use.
    pub ip_forgot_password_limiter: Arc<LoginRateLimiter>,
    /// The per-user and per-IP MFA limiters, exposed for the same reason.
    pub mfa_limiter: Arc<LoginRateLimiter>,
    pub ip_mfa_limiter: Arc<LoginRateLimiter>,
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
    setup_app_in_dir(collections, globals, config, tmp)
}

/// Like [`setup_app_with_config`] but uses a caller-provided config dir, so a
/// test can pre-populate it (e.g. write Lua hooks) before the hook runner loads.
pub fn setup_app_in_dir(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
    config: CrapConfig,
    tmp: tempfile::TempDir,
) -> TestApp {
    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");

    let shared = Registry::shared();
    {
        let mut reg = shared.write().unwrap();
        for def in &collections {
            reg.register_collection(def.clone());
        }
        for def in &globals {
            reg.register_global(def.clone());
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

    let translations = Arc::new(crap_cms::admin::translations::Translations::load(
        tmp.path(),
    ));
    let handlebars = templates::create_handlebars(tmp.path(), false, translations.clone(), None)
        .expect("create handlebars");

    let has_auth = registry
        .collections
        .values()
        .any(|d| d.is_auth_collection());

    let ip_forgot_password_limiter = Arc::new(LoginRateLimiter::new(20, 900));
    let mfa_limiter = Arc::new(LoginRateLimiter::new(5, 300));
    let ip_mfa_limiter = Arc::new(LoginRateLimiter::new(20, 300));
    let login_limiter = Arc::new(LoginRateLimiter::new(5, 300));

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
        login_limiter: Arc::clone(&login_limiter),
        ip_login_limiter: Arc::new(LoginRateLimiter::new(20, 300)),
        forgot_password_limiter: std::sync::Arc::new(
            crap_cms::core::rate_limit::LoginRateLimiter::new(3, 900),
        ),
        ip_forgot_password_limiter: Arc::clone(&ip_forgot_password_limiter),
        mfa_limiter: Arc::clone(&mfa_limiter),
        ip_mfa_limiter: Arc::clone(&ip_mfa_limiter),
        has_auth,
        translations,
        sse_slots: LiveSlots::new(0, 0),
        shutdown: CancellationToken::new(),
        password_provider: std::sync::Arc::new(crap_cms::core::auth::Argon2PasswordProvider),
        subscriber_send_timeout_ms: 1000,
        custom_pages: crap_cms::admin::custom_pages::CustomPageRegistry::default(),
    };

    let router = build_router(state);

    TestApp {
        _tmp: tmp,
        router,
        pool: db_pool,
        registry,
        jwt_secret: "test-jwt-secret".into(),
        ip_forgot_password_limiter,
        mfa_limiter,
        ip_mfa_limiter,
    }
}

pub fn create_test_user(app: &TestApp, email: &str, password: &str) -> String {
    create_test_user_with_role(app, email, password, "user")
}

pub fn create_test_user_with_role(
    app: &TestApp,
    email: &str,
    password: &str,
    role: &str,
) -> String {
    let reg = &app.registry;
    let def = reg.get_collection("users").unwrap().clone();

    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields = HashMap::from([
        ("email".to_string(), json!(email)),
        ("name".to_string(), json!("Test User")),
        ("role".to_string(), json!(role)),
    ])
    .into();
    let doc = query::create(&tx, "users", &def, &data, None).unwrap();
    query::update_password(&tx, "users", &doc.id, password).unwrap();
    tx.commit().unwrap();
    doc.id.to_string()
}

pub fn make_auth_cookie(app: &TestApp, user_id: &str, email: &str) -> String {
    // Read the user's current `_session_version` from the DB — the
    // evaluator's bearer/cookie path rejects JWTs whose claim doesn't
    // match (intentional: that's how password changes invalidate live
    // sessions). `create_test_user` calls `update_password` which
    // bumps the version to 1, so a default-built Claims (session_version
    // = 0) would be stale by construction and the cookie path would
    // return `Invalid(StaleSession)` → 303 redirect to /admin/login.
    let conn = app.pool.get().unwrap();
    let session_version = query::get_session_version(&conn, "users", user_id).unwrap();
    drop(conn);

    let claims = auth::Claims::builder(user_id, "users")
        .email(email)
        .exp((chrono::Utc::now().timestamp() as u64) + 3600)
        .session_version(session_version)
        .build()
        .unwrap();
    let token = auth::create_token(&claims, app.jwt_secret.as_ref()).unwrap();
    format!("crap_session={token}")
}

pub const TEST_CSRF: &str = "test-csrf-token-12345";

pub fn csrf_cookie() -> String {
    format!("crap_csrf={TEST_CSRF}")
}

pub fn auth_and_csrf(auth_cookie: &str) -> String {
    format!("{auth_cookie}; crap_csrf={TEST_CSRF}")
}

pub async fn body_string(body: Body) -> String {
    let bytes = body.collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

pub fn make_verify_users_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("vusers");
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
    def.auth = Some(Auth::enabled().map_password_login(|b| b.verify_email(true)));
    def
}

pub fn make_named_auth_def(slug: &str) -> CollectionDefinition {
    let mut def = CollectionDefinition::new(slug);
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("email", FieldType::Email)
            .required(true)
            .unique(true)
            .build(),
    ];
    def.auth = Some(Auth::enabled());
    def
}
