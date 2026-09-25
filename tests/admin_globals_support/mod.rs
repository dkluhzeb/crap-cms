//! Shared setup helpers for the `admin_globals*` integration test binaries.
//!
//! `tests/admin_globals_support/` is a subdirectory, so Cargo never compiles
//! it as a test binary of its own; each binary pulls it in with
//! `mod admin_globals_support;` and uses only the helpers its tests need.

#![allow(dead_code)]

use serde_json::json;
use std::{
    collections::HashMap,
    io::Cursor,
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::body::Body;
use http_body_util::BodyExt;
use image::{ExtendedColorType, ImageEncoder, codecs::png::PngEncoder};

use crap_cms::{
    admin::{AdminState, server::build_router, templates, translations::Translations},
    config::{CrapConfig, LocaleConfig},
    core::{
        DocumentFields, JwtSecret, Registry, auth,
        collection::{Auth, CollectionDefinition, GlobalDefinition, Labels, VersionsConfig},
        field::{FieldDefinition, FieldType, LocalizedString},
    },
    db::{migrate, pool, query},
    hooks,
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

pub fn make_global_def() -> GlobalDefinition {
    let mut def = GlobalDefinition::new("settings");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Settings".to_string())),
        plural: None,
    };
    def.fields = vec![FieldDefinition::builder("site_name", FieldType::Text).build()];
    def
}

pub struct TestApp {
    pub _tmp: tempfile::TempDir,
    pub router: axum::Router,
    pub pool: crap_cms::db::DbPool,
    pub registry: std::sync::Arc<crap_cms::core::Registry>,
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
    setup_app_inner(collections, globals, config, None)
}

/// Build a `TestApp` whose `HookRunner` loads collections, globals, and hooks
/// from `fixture_dir`. The programmatically-passed `collections` / `globals`
/// vecs are *additive* — they're registered on top of whatever the fixture's
/// `init_lua` already populated. This lets access-control tests use a real
/// Lua access hook while still driving the rest of the admin HTTP surface.
pub fn setup_app_with_fixture(fixture_dir: &Path) -> TestApp {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;
    setup_app_inner(
        Vec::new(),
        Vec::new(),
        config,
        Some(fixture_dir.to_path_buf()),
    )
}

pub fn setup_app_inner(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
    config: CrapConfig,
    fixture_dir: Option<PathBuf>,
) -> TestApp {
    let tmp = tempfile::tempdir().expect("tempdir");

    build_app(collections, globals, config, fixture_dir, tmp)
}

/// Like [`setup_app_with_config`] but uses a caller-provided config dir, so a
/// test can pre-populate it (e.g. write Lua access hooks) before the hook
/// runner loads.
pub fn setup_app_in_dir(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
    config: CrapConfig,
    tmp: tempfile::TempDir,
) -> TestApp {
    build_app(collections, globals, config, None, tmp)
}

fn build_app(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
    config: CrapConfig,
    fixture_dir: Option<PathBuf>,
    tmp: tempfile::TempDir,
) -> TestApp {
    // When a fixture dir is provided, initialize the registry by loading the
    // fixture's collections/globals/hooks via `hooks::init_lua` and then use
    // the fixture dir as the HookRunner's config_dir. Otherwise stick with the
    // programmatic registration path the rest of the suite relies on.
    let (shared, hook_config_dir) = match fixture_dir.as_deref() {
        Some(fd) => {
            let init_snap = hooks::init_lua(fd, &config).expect("init lua from fixture");
            let shared = Registry::shared();
            *shared.write().unwrap() = (*init_snap).clone();
            (shared, fd.to_path_buf())
        }
        None => (Registry::shared(), tmp.path().to_path_buf()),
    };

    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");

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
        .config_dir(&hook_config_dir)
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
        sse_connections: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        max_sse_connections: 0,
        shutdown: tokio_util::sync::CancellationToken::new(),
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
    }
}

pub fn create_test_user(app: &TestApp, email: &str, password: &str) -> String {
    create_test_user_with_role(app, email, password, None)
}

/// Create a test user with an optional `role` field — used by the admin-only
/// access-gate regression tests where the access hook reads `ctx.user.role`.
/// Skips the `role` field entirely when `None` so collections without a
/// `role` column (the common case) still work.
pub fn create_test_user_with_role(
    app: &TestApp,
    email: &str,
    password: &str,
    role: Option<&str>,
) -> String {
    let def = app.registry.get_collection("users").unwrap().clone();

    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let mut data: DocumentFields = HashMap::from([
        ("email".to_string(), json!(email)),
        ("name".to_string(), json!("Test User")),
    ])
    .into();
    if let Some(r) = role {
        data.insert("role".to_string(), json!(r));
    }
    let doc = query::create(&tx, "users", &def, &data, None).unwrap();
    query::update_password(&tx, "users", &doc.id, password).unwrap();
    tx.commit().unwrap();
    doc.id.to_string()
}

pub fn make_auth_cookie(app: &TestApp, user_id: &str, email: &str) -> String {
    // Read the user's current session_version from the DB. `query::update_password`
    // bumps this to 1 the moment a password is set, so a Claims with the default
    // session_version = 0 would be rejected by `auth_middleware::load_auth_user`
    // and ctx.user would be nil in downstream hooks.
    let conn = app.pool.get().unwrap();
    let session_version = query::auth::get_session_version(&conn, "users", user_id).unwrap_or(0);
    let claims = auth::Claims::builder(user_id, "users")
        .email(email)
        .session_version(session_version)
        .exp((chrono::Utc::now().timestamp() as u64) + 3600)
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

pub fn make_locale_config() -> LocaleConfig {
    LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    }
}

pub fn make_versioned_global_def() -> GlobalDefinition {
    let mut def = GlobalDefinition::new("site_config");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Site Config".to_string())),
        plural: None,
    };
    def.fields = vec![
        FieldDefinition::builder("site_name", FieldType::Text).build(),
        FieldDefinition::builder("tagline", FieldType::Text).build(),
    ];
    def.versions = Some(VersionsConfig::new(true, 10));
    def
}

pub fn make_localized_global_def() -> GlobalDefinition {
    let mut def = GlobalDefinition::new("l10n_settings");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("L10N Settings".to_string())),
        plural: None,
    };
    def.fields = vec![
        FieldDefinition::builder("welcome_text", FieldType::Text)
            .localized(true)
            .build(),
        FieldDefinition::builder("max_items", FieldType::Number).build(),
    ];
    def
}

/// A 1x1 transparent PNG.
pub fn tiny_png() -> Vec<u8> {
    let mut buf = Cursor::new(Vec::new());
    let encoder = PngEncoder::new(&mut buf);
    encoder
        .write_image(&[0u8, 0, 0, 0], 1, 1, ExtendedColorType::Rgba8)
        .unwrap();
    buf.into_inner()
}
