use std::{collections::HashMap, sync::Arc};

use axum::body::Body;
use http_body_util::BodyExt;
use serde_json::json;

use crap_cms::{
    admin::{AdminState, server::build_router, templates, translations::Translations},
    config::CrapConfig,
    config::EmailConfig,
    core::{
        DocumentFields, JwtSecret, Registry, auth,
        collection::*,
        email::{EmailRenderer, create_email_provider},
        field::*,
    },
    db::{migrate, pool, query},
    hooks::lifecycle::HookRunner,
};

pub struct TestApp {
    pub _tmp: tempfile::TempDir,
    pub router: axum::Router,
    pub pool: crap_cms::db::DbPool,
    pub registry: std::sync::Arc<crap_cms::core::Registry>,
    pub jwt_secret: JwtSecret,
}

pub const TEST_CSRF: &str = "test-csrf-token-12345";

pub fn setup_app(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
) -> TestApp {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;
    // Test server is HTTP — disable `Secure` flag on session cookies so
    // chromiumoxide can store them. (Without this the browser drops the
    // session cookie on HTTP and every authenticated request 401's.)
    config.admin.dev_mode = true;
    setup_app_with_config(collections, globals, config)
}

/// Like [`setup_app`] but writes a set of `access/*.lua` files into the
/// test config directory before initializing the Lua VM. Each entry is
/// `(name, lua_source)` — the name maps to `access/<name>.lua` and is
/// referenced from collection definitions as `"access.<name>"`. Use this
/// to exercise access-gating end-to-end.
pub fn setup_app_with_access_files(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
    access_files: &[(&str, &str)],
) -> TestApp {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;
    config.admin.dev_mode = true;

    let tmp = tempfile::tempdir().expect("tempdir");
    let access_dir = tmp.path().join("access");
    std::fs::create_dir_all(&access_dir).expect("create access dir");

    for (name, src) in access_files {
        std::fs::write(access_dir.join(format!("{name}.lua")), src)
            .unwrap_or_else(|e| panic!("write access/{name}.lua: {e}"));
    }

    setup_app_at(collections, globals, config, tmp)
}

pub fn setup_app_with_config(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
    config: CrapConfig,
) -> TestApp {
    let tmp = tempfile::tempdir().expect("tempdir");
    setup_app_at(collections, globals, config, tmp)
}

/// Like [`setup_app_with_config`] but takes an externally-prepared
/// `tempdir`. Lets callers pre-populate it with `access/`, `hooks/`,
/// or `init.lua` files that Lua needs to resolve during init.
pub fn setup_app_at(
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

    let translations = Arc::new(Translations::load(tmp.path()));
    let handlebars = templates::create_handlebars(tmp.path(), false, translations.clone(), None)
        .expect("create handlebars");
    let email_renderer = Arc::new(EmailRenderer::new(tmp.path()).expect("create email renderer"));

    let has_auth = registry
        .collections
        .values()
        .any(crap_cms::core::CollectionDefinition::is_auth_collection);

    let state = AdminState {
        config,
        config_dir: tmp.path().to_path_buf(),
        pool: db_pool.clone(),
        registry: Arc::clone(&registry),
        handlebars,
        hook_runner,
        jwt_secret: "test-jwt-secret".into(),
        email_renderer,
        email_provider: create_email_provider(&EmailConfig::default()).unwrap(),
        event_transport: None,
        login_limiter: Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(5, 300)),
        ip_login_limiter: Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(20, 300)),
        forgot_password_limiter: Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
            3, 900,
        )),
        ip_forgot_password_limiter: Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
            20, 900,
        )),
        has_auth,
        translations,
        sse_connections: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        max_sse_connections: 0,
        shutdown: tokio_util::sync::CancellationToken::new(),
        storage: crap_cms::core::upload::create_storage(
            tmp.path(),
            &crap_cms::config::UploadConfig::default(),
        )
        .unwrap(),
        // Must match `jwt_secret` above — the login handler signs JWTs with the
        // token_provider, and the auth middleware verifies with `jwt_secret`.
        // If these diverge, every authenticated request 401's because the
        // signature won't validate.
        token_provider: std::sync::Arc::new(crap_cms::core::auth::JwtTokenProvider::new(
            "test-jwt-secret",
        )),
        password_provider: std::sync::Arc::new(crap_cms::core::auth::Argon2PasswordProvider),
        subscriber_send_timeout_ms: 1000,
        invalidation_transport: std::sync::Arc::new(
            crap_cms::core::event::InProcessInvalidationBus::new(),
        ),
        populate_singleflight: std::sync::Arc::new(query::Singleflight::new()),
        cache: None,
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
    let def = app.registry.get_collection("users").unwrap().clone();

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

/// Like [`create_test_user`] but also sets `role` — for access-fn
/// tests that gate on `context.user.role`. Assumes the users
/// collection has a `role` field defined (see [`make_users_def_with_role`]).
pub fn create_test_user_with_role(
    app: &TestApp,
    email: &str,
    password: &str,
    role: &str,
) -> String {
    let def = app.registry.get_collection("users").unwrap().clone();

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
    // Read the user's current `_session_version` from the DB and include it
    // in the JWT — `auth_middleware::load_auth_user` rejects tokens whose
    // session_version differs from the DB. `update_password` increments it
    // to 1, so a default-built JWT (sv=0) would otherwise fail to auth.
    let conn = app.pool.get().expect("pool");
    let ctx = crap_cms::service::ServiceContext::slug_only("users")
        .conn(&conn)
        .build();
    let session_version = crap_cms::service::auth::get_session_version(&ctx, user_id).unwrap_or(0);

    let claims = auth::Claims::builder(user_id, "users")
        .email(email)
        .session_version(session_version)
        .exp((chrono::Utc::now().timestamp() as u64) + 3600)
        .build()
        .unwrap();
    let token = auth::create_token(&claims, app.jwt_secret.as_ref()).unwrap();
    format!("crap_session={token}")
}

pub fn auth_and_csrf(auth_cookie: &str) -> String {
    format!("{auth_cookie}; crap_csrf={TEST_CSRF}")
}

pub async fn body_string(body: Body) -> String {
    let bytes = body.collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

// ── Common definition builders ────────────────────────────────────────────

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
    // `Auth::enabled()` seeds the methods list with the standard
    // `password_login` + `bearer` + `session_cookie` set. The struct-
    // literal form `Auth { enabled: true, ..Default::default() }`
    // leaves `methods = vec![]`, which the methods-driven login page
    // interprets as "no password login configured" → renders the
    // disable_local message instead of the email/password form, and
    // every `browser_login` call then fails with
    // `Could not find input[name=email]`.
    def.auth = Some(Auth::enabled());
    def
}

/// `users` collection with a `role` field — for access-fn tests that
/// gate on `context.user.role`.
pub fn make_users_def_with_role() -> CollectionDefinition {
    let mut def = make_users_def();
    def.fields
        .push(FieldDefinition::builder("role", FieldType::Text).build());
    def
}

pub fn make_settings_def() -> GlobalDefinition {
    let mut def = GlobalDefinition::new("settings");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Settings".to_string())),
        plural: None,
    };
    def.fields = vec![FieldDefinition::builder("site_name", FieldType::Text).build()];
    def
}

pub struct HtmlTestCtx {
    pub app: TestApp,
    pub user_id: String,
    pub cookie: String,
}

pub fn setup_html_test(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
    email: &str,
    password: &str,
) -> HtmlTestCtx {
    let app = setup_app(collections, globals);
    let user_id = create_test_user(&app, email, password);
    let cookie = make_auth_cookie(&app, &user_id, email);
    HtmlTestCtx {
        app,
        user_id,
        cookie,
    }
}

pub fn setup_html_test_with_config(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
    config: CrapConfig,
    email: &str,
    password: &str,
) -> HtmlTestCtx {
    let app = setup_app_with_config(collections, globals, config);
    let user_id = create_test_user(&app, email, password);
    let cookie = make_auth_cookie(&app, &user_id, email);
    HtmlTestCtx {
        app,
        user_id,
        cookie,
    }
}

pub fn setup_html_test_with_access_files(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
    access_files: &[(&str, &str)],
    email: &str,
    password: &str,
) -> HtmlTestCtx {
    let app = setup_app_with_access_files(collections, globals, access_files);
    let user_id = create_test_user(&app, email, password);
    let cookie = make_auth_cookie(&app, &user_id, email);
    HtmlTestCtx {
        app,
        user_id,
        cookie,
    }
}
