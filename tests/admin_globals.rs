//! Globals-related integration tests for admin HTTP handlers.
//!
//! Covers: global CRUD, versioning, locale, drafts, upload serving,
//! static assets, dashboard, CSRF, CORS, access gate.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use crap_cms::admin::AdminState;
use crap_cms::admin::server::build_router;
use crap_cms::admin::templates;
use crap_cms::admin::translations::Translations;
use crap_cms::config::{CrapConfig, LocaleConfig};
use crap_cms::core::auth;
use crap_cms::core::collection::*;
use crap_cms::core::email::EmailRenderer;
use crap_cms::core::field::*;
use crap_cms::core::{JwtSecret, Registry};
use crap_cms::db::{migrate, pool, query};
use crap_cms::hooks::lifecycle::HookRunner;

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

struct TestApp {
    _tmp: tempfile::TempDir,
    router: axum::Router,
    pool: crap_cms::db::DbPool,
    registry: crap_cms::core::SharedRegistry,
    jwt_secret: JwtSecret,
}

fn setup_app(collections: Vec<CollectionDefinition>, globals: Vec<GlobalDefinition>) -> TestApp {
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;
    setup_app_with_config(collections, globals, config)
}

fn setup_app_with_config(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
    config: CrapConfig,
) -> TestApp {
    let tmp = tempfile::tempdir().expect("tempdir");

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

    let translations = Arc::new(Translations::load(tmp.path()));
    let handlebars = templates::create_handlebars(tmp.path(), false, translations.clone())
        .expect("create handlebars");
    let email_renderer = Arc::new(EmailRenderer::new(tmp.path()).expect("create email renderer"));

    let has_auth = {
        let reg = registry.read().unwrap();
        reg.collections.values().any(|d| d.is_auth_collection())
    };

    let state = AdminState {
        config,
        config_dir: tmp.path().to_path_buf(),
        pool: db_pool.clone(),
        registry: Registry::snapshot(&registry),
        handlebars,
        hook_runner,
        jwt_secret: "test-jwt-secret".into(),
        email_renderer,
        event_bus: None,
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
        has_auth,
        translations,
        sse_connections: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        max_sse_connections: 0,
        shutdown: tokio_util::sync::CancellationToken::new(),
        csp_header: None,
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

fn create_test_user(app: &TestApp, email: &str, password: &str) -> String {
    let reg = app.registry.read().unwrap();
    let def = reg.get_collection("users").unwrap().clone();
    drop(reg);

    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([
        ("email".to_string(), email.to_string()),
        ("name".to_string(), "Test User".to_string()),
    ]);
    let doc = query::create(&tx, "users", &def, &data, None).unwrap();
    query::update_password(&tx, "users", &doc.id, password).unwrap();
    tx.commit().unwrap();
    doc.id.to_string()
}

fn make_auth_cookie(app: &TestApp, user_id: &str, email: &str) -> String {
    let claims = auth::Claims::builder(user_id, "users")
        .email(email)
        .exp((chrono::Utc::now().timestamp() as u64) + 3600)
        .build()
        .unwrap();
    let token = auth::create_token(&claims, app.jwt_secret.as_ref()).unwrap();
    format!("crap_session={}", token)
}

const TEST_CSRF: &str = "test-csrf-token-12345";

fn auth_and_csrf(auth_cookie: &str) -> String {
    format!("{}; crap_csrf={}", auth_cookie, TEST_CSRF)
}

async fn body_string(body: Body) -> String {
    let bytes = body.collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn make_locale_config() -> LocaleConfig {
    LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    }
}

fn make_versioned_global_def() -> GlobalDefinition {
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

fn make_localized_global_def() -> GlobalDefinition {
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

// ── 1D. Globals ───────────────────────────────────────────────────────────

#[tokio::test]
async fn global_edit_form_returns_200() {
    let app = setup_app(vec![make_users_def()], vec![make_global_def()]);
    let user_id = create_test_user(&app, "global@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "global@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/globals/settings")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn global_update_action() {
    let app = setup_app(vec![make_users_def()], vec![make_global_def()]);
    let user_id = create_test_user(&app, "global_update@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "global_update@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/globals/settings")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("site_name=My+CMS"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND || status == StatusCode::OK,
        "Global update should redirect or HX-Redirect, got {}",
        status
    );
}

// ── Global Handler Gaps ───────────────────────────────────────────────────

#[tokio::test]
async fn global_update_returns_redirect() {
    let app = setup_app(vec![make_users_def()], vec![make_global_def()]);
    let user_id = create_test_user(&app, "global_redir@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "global_redir@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/globals/settings")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("site_name=Updated+Site"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND || status == StatusCode::OK,
        "Global update should redirect or HX-Redirect, got {}",
        status
    );
}

// ── Global Versioning Tests ──────────────────────────────────────────────

#[tokio::test]
async fn global_versions_page_returns_200() {
    let app = setup_app(vec![make_users_def()], vec![make_versioned_global_def()]);
    let user_id = create_test_user(&app, "gv@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "gv@test.com");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/globals/site_config")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("site_name=Test+Site&tagline=Hello"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "Global update should succeed, got {}",
        status
    );

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/globals/site_config/versions")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(
        body.to_lowercase().contains("version") || body.to_lowercase().contains("history"),
        "Versions page should contain version-related content"
    );
}

#[tokio::test]
async fn global_versions_page_non_versioned_redirects() {
    let app = setup_app(vec![make_users_def()], vec![make_global_def()]);
    let user_id = create_test_user(&app, "gvr@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "gvr@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/globals/settings/versions")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER
            || status == StatusCode::FOUND
            || status == StatusCode::TEMPORARY_REDIRECT,
        "Non-versioned global versions page should redirect, got {}",
        status
    );
}

#[tokio::test]
async fn global_nonexistent_returns_404() {
    let app = setup_app(vec![make_users_def()], vec![make_global_def()]);
    let user_id = create_test_user(&app, "gnf@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "gnf@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/globals/nonexistent")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn global_update_with_draft_action() {
    let app = setup_app(vec![make_users_def()], vec![make_versioned_global_def()]);
    let user_id = create_test_user(&app, "gdraft@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "gdraft@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/globals/site_config")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(
                    "site_name=Draft+Site&tagline=WIP&_action=save_draft",
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "Draft save should succeed, got {}",
        status
    );
}

#[tokio::test]
async fn global_update_unpublish_action() {
    let app = setup_app(vec![make_users_def()], vec![make_versioned_global_def()]);
    let user_id = create_test_user(&app, "gunpub@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "gunpub@test.com");

    let _resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/globals/site_config")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("site_name=Published+Site&tagline=Live"))
                .unwrap(),
        )
        .await
        .unwrap();

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/globals/site_config")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(
                    "site_name=Published+Site&tagline=Live&_action=unpublish",
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "Unpublish should succeed, got {}",
        status
    );
}

#[tokio::test]
async fn global_restore_version() {
    let app = setup_app(vec![make_users_def()], vec![make_versioned_global_def()]);
    let user_id = create_test_user(&app, "grestore@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "grestore@test.com");

    let _resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/globals/site_config")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("site_name=Version+1&tagline=First"))
                .unwrap(),
        )
        .await
        .unwrap();

    let _resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/globals/site_config")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("site_name=Version+2&tagline=Second"))
                .unwrap(),
        )
        .await
        .unwrap();

    let conn = app.pool.get().unwrap();
    let versions = query::list_versions(&conn, "_global_site_config", "default", Some(10), None)
        .unwrap_or_default();
    drop(conn);

    if let Some(v) = versions.first() {
        let resp = app
            .router
            .clone()
            .oneshot(
                Request::post(format!(
                    "/admin/globals/site_config/versions/{}/restore",
                    v.id
                ))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        assert!(
            status == StatusCode::SEE_OTHER || status == StatusCode::OK,
            "Restore should succeed, got {}",
            status
        );
    }
}

#[tokio::test]
async fn global_restore_non_versioned_redirects() {
    let app = setup_app(vec![make_users_def()], vec![make_global_def()]);
    let user_id = create_test_user(&app, "gnvr@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "gnvr@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/globals/settings/versions/fake-version-id/restore")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER
            || status == StatusCode::FOUND
            || status == StatusCode::TEMPORARY_REDIRECT
            || status == StatusCode::OK,
        "Non-versioned restore should redirect, got {}",
        status
    );
}

// ── Localized Global Tests ───────────────────────────────────────────────

#[tokio::test]
async fn localized_global_edit_returns_200() {
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.locale = make_locale_config();
    let app = setup_app_with_config(
        vec![make_users_def()],
        vec![make_localized_global_def()],
        config,
    );
    let user_id = create_test_user(&app, "lglobal@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "lglobal@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/globals/l10n_settings?locale=en")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn localized_global_edit_non_default_locale() {
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.locale = make_locale_config();
    let app = setup_app_with_config(
        vec![make_users_def()],
        vec![make_localized_global_def()],
        config,
    );
    let user_id = create_test_user(&app, "lglobal2@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "lglobal2@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/globals/l10n_settings")
                .header("cookie", format!("{}; crap_editor_locale=de", &cookie))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn localized_global_update_with_locale() {
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.locale = make_locale_config();
    let app = setup_app_with_config(
        vec![make_users_def()],
        vec![make_localized_global_def()],
        config,
    );
    let user_id = create_test_user(&app, "lglobal3@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "lglobal3@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/globals/l10n_settings")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(
                    "welcome_text=Willkommen&max_items=10&_locale=de",
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "Localized global update should succeed, got {}",
        status
    );
}

// ── Dashboard with Globals ───────────────────────────────────────────────

#[tokio::test]
async fn dashboard_shows_globals() {
    let app = setup_app(vec![make_users_def()], vec![make_global_def()]);
    let user_id = create_test_user(&app, "dashglobal@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "dashglobal@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let body_lower = body.to_lowercase();
    assert!(
        body_lower.contains("settings"),
        "Dashboard should show global cards"
    );
}

// ── Global update on nonexistent global ──────────────────────────────────

#[tokio::test]
async fn global_update_nonexistent_redirects() {
    let app = setup_app(vec![make_users_def()], vec![make_global_def()]);
    let user_id = create_test_user(&app, "gupdnf@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "gupdnf@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/globals/nonexistent_global")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("site_name=Test"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER
            || status == StatusCode::FOUND
            || status == StatusCode::TEMPORARY_REDIRECT,
        "Update nonexistent global should redirect, got {}",
        status
    );
}

// ── Static / Dashboard Gaps ───────────────────────────────────────────────

#[tokio::test]
async fn static_asset_missing_returns_404() {
    let app = setup_app(vec![make_posts_def()], vec![]);
    let resp = app
        .router
        .oneshot(
            Request::get("/static/nonexistent.css")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "Non-existent static asset should return 404"
    );
}

#[tokio::test]
async fn dashboard_renders_collection_counts() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "dashcount@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "dashcount@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    for title in &["Post A", "Post B"] {
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let data = std::collections::HashMap::from([("title".to_string(), title.to_string())]);
        query::create(&tx, "posts", &def, &data, None).unwrap();
        tx.commit().unwrap();
    }

    let resp = app
        .router
        .oneshot(
            Request::get("/admin")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let body_lower = body.to_lowercase();
    assert!(
        body_lower.contains("posts") || body_lower.contains("post"),
        "Dashboard should contain collection info"
    );
}

// ── Globals: Update with locale ───────────────────────────────────────────
