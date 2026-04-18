//! Globals locale/versioned/draft, upload serving, dashboard variants,
//! CSRF, CORS, access gate tests for admin HTTP handlers.

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
    let mut config = CrapConfig::test_default();
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
        email_provider: crap_cms::core::email::create_email_provider(
            &crap_cms::config::EmailConfig::default(),
        )
        .unwrap(),
        event_transport: None,
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
        storage: crap_cms::core::upload::create_storage(
            tmp.path(),
            &crap_cms::config::UploadConfig::default(),
        )
        .unwrap(),
        token_provider: std::sync::Arc::new(crap_cms::core::auth::JwtTokenProvider::new(
            "test-secret",
        )),
        password_provider: std::sync::Arc::new(crap_cms::core::auth::Argon2PasswordProvider),
        subscriber_send_timeout_ms: 1000,
        invalidation_transport: std::sync::Arc::new(
            crap_cms::core::event::InProcessInvalidationBus::new(),
        ),
        populate_singleflight: std::sync::Arc::new(crap_cms::db::query::Singleflight::new()),
        cache: None,
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

fn csrf_cookie() -> String {
    format!("crap_csrf={}", TEST_CSRF)
}

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

fn tiny_png() -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    let encoder = image::codecs::png::PngEncoder::new(&mut buf);
    use image::ImageEncoder;
    encoder
        .write_image(&[0u8, 0, 0, 0], 1, 1, image::ExtendedColorType::Rgba8)
        .unwrap();
    buf.into_inner()
}

// ── Globals: Update with locale ───────────────────────────────────────────

#[tokio::test]
async fn global_update_with_locale() {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.locale = make_locale_config();

    let app = setup_app_with_config(
        vec![make_users_def()],
        vec![make_localized_global_def()],
        config,
    );
    let user_id = create_test_user(&app, "globalloc@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "globalloc@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/globals/l10n_settings")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(
                    "site_title=Localized+Title&description=Desc&_locale=de",
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "Global update with locale should succeed, got {}",
        status
    );
}

// ── Globals: Versioned ────────────────────────────────────────────────────

#[tokio::test]
async fn global_versioned_edit_shows_version_info() {
    let app = setup_app(vec![make_users_def()], vec![make_versioned_global_def()]);
    let user_id = create_test_user(&app, "gver@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "gver@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/globals/site_config")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(
        body.to_lowercase().contains("version") || body.contains("draft"),
        "Versioned global edit should show version/draft info"
    );
}

#[tokio::test]
async fn global_unpublish() {
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
                .body(Body::from("site_name=My+Site"))
                .unwrap(),
        )
        .await
        .unwrap();

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/globals/site_config")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("site_name=My+Site&_action=unpublish"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "Global unpublish should succeed, got {}",
        status
    );
}

#[tokio::test]
async fn global_version_history_page() {
    let app = setup_app(vec![make_users_def()], vec![make_versioned_global_def()]);
    let user_id = create_test_user(&app, "gverpage@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "gverpage@test.com");

    let resp = app
        .router
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
        body.to_lowercase().contains("version"),
        "Global versions page should contain 'version'"
    );
}

#[tokio::test]
async fn global_non_versioned_versions_page_redirects() {
    let app = setup_app(vec![make_users_def()], vec![make_global_def()]);
    let user_id = create_test_user(&app, "gnv@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "gnv@test.com");

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
async fn global_restore_version_non_versioned_redirects() {
    let app = setup_app(vec![make_users_def()], vec![make_global_def()]);
    let user_id = create_test_user(&app, "grest@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "grest@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/globals/settings/versions/fake-version/restore")
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
        "Global restore on non-versioned should redirect, got {}",
        status
    );
}

// ── Upload Serving Tests ─────────────────────────────────────────────────

#[tokio::test]
async fn serve_upload_nonexistent_returns_404() {
    let app = setup_app(vec![make_posts_def()], vec![]);
    let resp = app
        .router
        .oneshot(
            Request::get("/uploads/posts/nofile.jpg")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn serve_upload_path_traversal_returns_404() {
    let app = setup_app(vec![make_posts_def()], vec![]);
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/uploads/posts/../../etc/passwd")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn serve_upload_existing_file() {
    let app = setup_app(vec![make_posts_def()], vec![]);

    let upload_dir = app._tmp.path().join("uploads").join("posts");
    std::fs::create_dir_all(&upload_dir).unwrap();
    std::fs::write(upload_dir.join("test.txt"), b"hello world").unwrap();

    let resp = app
        .router
        .oneshot(
            Request::get("/uploads/posts/test.txt")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap_or(""))
        .unwrap_or("");
    assert!(
        ct.contains("text/plain"),
        "Should detect text/plain MIME, got {}",
        ct
    );
    let cache = resp
        .headers()
        .get("cache-control")
        .map(|v| v.to_str().unwrap_or(""))
        .unwrap_or("");
    assert!(
        cache.contains("public"),
        "Public file should have public cache control, got {}",
        cache
    );
    let body = body_string(resp.into_body()).await;
    assert_eq!(body, "hello world");
}

#[tokio::test]
async fn serve_upload_image_file() {
    let app = setup_app(vec![make_posts_def()], vec![]);

    let upload_dir = app._tmp.path().join("uploads").join("posts");
    std::fs::create_dir_all(&upload_dir).unwrap();
    let png_data = tiny_png();
    std::fs::write(upload_dir.join("image.png"), &png_data).unwrap();

    let resp = app
        .router
        .oneshot(
            Request::get("/uploads/posts/image.png")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap_or(""))
        .unwrap_or("");
    assert!(
        ct.contains("image/png"),
        "Should detect image/png MIME, got {}",
        ct
    );
}

#[tokio::test]
async fn serve_upload_path_traversal_blocked() {
    let app = setup_app(vec![make_posts_def()], vec![]);

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/uploads/posts/../../../etc/passwd")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn upload_path_traversal_returns_404() {
    let app = setup_app(vec![make_posts_def()], vec![]);

    let resp = app
        .router
        .oneshot(
            Request::get("/uploads/posts/../../../etc/passwd")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn upload_path_traversal_in_collection_returns_404() {
    let app = setup_app(vec![make_posts_def()], vec![]);

    let resp = app
        .router
        .oneshot(
            Request::get("/uploads/..%2F..%2Fetc/passwd")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::NOT_FOUND || status == StatusCode::BAD_REQUEST,
        "Path traversal should be rejected, got {}",
        status
    );
}

// ── Dashboard: No collections ─────────────────────────────────────────────

#[tokio::test]
async fn dashboard_no_auth_no_collections() {
    let app = setup_app(vec![], vec![]);
    let resp = app
        .router
        .oneshot(Request::get("/admin").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ── No auth (no auth collections) dashboard ──────────────────────────────

#[tokio::test]
async fn dashboard_no_auth_returns_200() {
    let app = setup_app(vec![make_posts_def()], vec![make_global_def()]);
    let resp = app
        .router
        .oneshot(Request::get("/admin").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let body_lower = body.to_lowercase();
    assert!(
        body_lower.contains("dashboard")
            || body_lower.contains("posts")
            || body_lower.contains("settings"),
        "Dashboard should render without auth"
    );
}

// ── Static file serving: JS and font ─────────────────────────────────────

#[tokio::test]
async fn static_js_returns_200() {
    let app = setup_app(vec![make_posts_def()], vec![]);
    let resp = app
        .router
        .oneshot(
            Request::get("/static/components/index.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap_or(""));
    assert!(
        ct.unwrap_or("").contains("javascript"),
        "JS content type should be javascript, got {:?}",
        ct
    );
}

// ── Globals: Save as draft ─────────────────────────────────────────────────

#[tokio::test]
async fn global_save_as_draft() {
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
                .body(Body::from("site_name=Draft+Value&_action=save_draft"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "Global save as draft should succeed, got {}",
        status
    );
}

#[tokio::test]
async fn global_edit_nonexistent_returns_404() {
    let app = setup_app(vec![make_users_def()], vec![make_global_def()]);
    let user_id = create_test_user(&app, "globnon@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "globnon@test.com");

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
async fn versioned_global_edit_returns_200() {
    let app = setup_app(vec![make_users_def()], vec![make_versioned_global_def()]);
    let user_id = create_test_user(&app, "verglobal@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "verglobal@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/globals/site_config")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn versioned_global_update_as_draft() {
    let app = setup_app(vec![make_users_def()], vec![make_versioned_global_def()]);
    let user_id = create_test_user(&app, "vergdraft@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "vergdraft@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/globals/site_config")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("site_title=Draft+Title&_action=save_draft"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "Global draft save should succeed, got {}",
        status
    );
}

#[tokio::test]
async fn versioned_global_versions_page() {
    let app = setup_app(vec![make_users_def()], vec![make_versioned_global_def()]);
    let user_id = create_test_user(&app, "vergver@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "vergver@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/globals/site_config/versions")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn non_versioned_global_versions_page_redirects() {
    let app = setup_app(vec![make_users_def()], vec![make_global_def()]);
    let user_id = create_test_user(&app, "nonverglob@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "nonverglob@test.com");

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
    assert_eq!(
        status,
        StatusCode::SEE_OTHER,
        "Non-versioned global versions page should redirect"
    );
}

#[tokio::test]
async fn global_restore_nonversioned_redirects() {
    let app = setup_app(vec![make_users_def()], vec![make_global_def()]);
    let user_id = create_test_user(&app, "grestnv@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "grestnv@test.com");

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
    assert_eq!(
        status,
        StatusCode::SEE_OTHER,
        "Restore on non-versioned global should redirect"
    );
}

// ── Global: Edit form with locale ─────────────────────────────────────────

#[tokio::test]
async fn global_edit_with_locale() {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.locale = make_locale_config();

    let app = setup_app_with_config(
        vec![make_users_def()],
        vec![make_localized_global_def()],
        config,
    );
    let user_id = create_test_user(&app, "geditloc@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "geditloc@test.com");

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
    let body = body_string(resp.into_body()).await;
    assert!(
        body.contains("DE") || body.contains("de"),
        "Should show locale selector"
    );
}

// ── CSRF Protection Tests ─────────────────────────────────────────────

#[tokio::test]
async fn csrf_post_without_token_returns_403() {
    let app = setup_app(vec![make_users_def()], vec![]);

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                    [127, 0, 0, 1],
                    0,
                ))))
                .body(Body::from("collection=users&email=a@b.com&password=x"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "POST without CSRF token should be 403"
    );
}

#[tokio::test]
async fn csrf_post_with_cookie_but_no_header_returns_403() {
    let app = setup_app(vec![make_users_def()], vec![]);

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/login")
                .header("Cookie", csrf_cookie())
                .header("content-type", "application/x-www-form-urlencoded")
                .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                    [127, 0, 0, 1],
                    0,
                ))))
                .body(Body::from("collection=users&email=a@b.com&password=x"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "POST with cookie but no token should be 403"
    );
}

#[tokio::test]
async fn csrf_post_with_mismatched_header_returns_403() {
    let app = setup_app(vec![make_users_def()], vec![]);

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/login")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", "wrong-token-value")
                .header("content-type", "application/x-www-form-urlencoded")
                .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                    [127, 0, 0, 1],
                    0,
                ))))
                .body(Body::from("collection=users&email=a@b.com&password=x"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "POST with mismatched CSRF header should be 403"
    );
}

#[tokio::test]
async fn csrf_post_with_matching_header_passes() {
    let app = setup_app(vec![make_users_def()], vec![]);

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/login")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                    [127, 0, 0, 1],
                    0,
                ))))
                .body(Body::from("collection=users&email=a@b.com&password=wrong"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "POST with matching CSRF header should not be 403"
    );
}

#[tokio::test]
async fn csrf_post_with_form_field_passes() {
    let app = setup_app(vec![make_users_def()], vec![]);

    let body = format!(
        "collection=users&email=a@b.com&password=wrong&_csrf={}",
        TEST_CSRF
    );
    let resp = app
        .router
        .oneshot(
            Request::post("/admin/login")
                .header("Cookie", csrf_cookie())
                .header("content-type", "application/x-www-form-urlencoded")
                .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                    [127, 0, 0, 1],
                    0,
                ))))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "POST with _csrf form field should not be 403"
    );
}

#[tokio::test]
async fn csrf_get_sets_cookie() {
    let app = setup_app(vec![make_users_def()], vec![]);

    let resp = app
        .router
        .oneshot(Request::get("/admin/login").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let set_cookie = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with("crap_csrf="));
    assert!(
        set_cookie.is_some(),
        "GET response should set crap_csrf cookie"
    );
    let cookie_val = set_cookie.unwrap();
    assert!(
        cookie_val.contains("SameSite=Strict"),
        "CSRF cookie should be SameSite=Strict"
    );
    assert!(
        !cookie_val.contains("HttpOnly"),
        "CSRF cookie must NOT be HttpOnly (JS needs to read it)"
    );
}

#[tokio::test]
async fn csrf_delete_without_token_returns_403() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "csrfdelete@test.com", "pass123");
    let auth_cookie = make_auth_cookie(&app, &user_id, "csrfdelete@test.com");

    let resp = app
        .router
        .oneshot(
            Request::delete("/admin/collections/posts/some-id")
                .header("Cookie", &auth_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "DELETE without CSRF should be 403"
    );
}

// ── CORS tests ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn cors_disabled_by_default() {
    let app = setup_app(vec![make_posts_def()], vec![]);

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/login")
                .header("Origin", "http://evil.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        resp.headers().get("access-control-allow-origin").is_none(),
        "No CORS headers when cors is not configured"
    );
}

#[tokio::test]
async fn cors_preflight_returns_headers() {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.cors.allowed_origins = vec!["http://localhost:8080".to_string()];

    let app = setup_app_with_config(vec![make_posts_def()], vec![], config);

    let resp = app
        .router
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/admin/login")
                .header("Origin", "http://localhost:8080")
                .header("Access-Control-Request-Method", "POST")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.headers()
            .get("access-control-allow-origin")
            .map(|v| v.to_str().unwrap()),
        Some("http://localhost:8080"),
        "Preflight should return matching origin"
    );
}

#[tokio::test]
async fn cors_wildcard_returns_star() {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.cors.allowed_origins = vec!["*".to_string()];

    let app = setup_app_with_config(vec![make_posts_def()], vec![], config);

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/login")
                .header("Origin", "http://anything.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.headers()
            .get("access-control-allow-origin")
            .map(|v| v.to_str().unwrap()),
        Some("*"),
        "Wildcard origin should return *"
    );
}

#[tokio::test]
async fn cors_non_matching_origin_not_reflected() {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.cors.allowed_origins = vec!["http://allowed.com".to_string()];

    let app = setup_app_with_config(vec![make_posts_def()], vec![], config);

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/login")
                .header("Origin", "http://not-allowed.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        resp.headers().get("access-control-allow-origin").is_none(),
        "Non-matching origin should not get CORS header"
    );
}

// ── Admin Access Gate Tests ─────────────────────────────────────────────

#[tokio::test]
async fn require_auth_blocks_when_no_auth_collection() {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = true;

    let app = setup_app_with_config(vec![make_posts_def()], vec![], config);
    let resp = app
        .router
        .oneshot(Request::get("/admin").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "require_auth=true with no auth collection should return 503"
    );
    let body = body_string(resp.into_body()).await;
    assert!(
        body.contains("Setup Required") || body.contains("setup required") || body.contains("auth"),
        "Response should mention setup/auth requirement"
    );
}

#[tokio::test]
async fn require_auth_false_allows_open_admin() {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;

    let app = setup_app_with_config(vec![make_posts_def()], vec![], config);
    let resp = app
        .router
        .oneshot(Request::get("/admin").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "require_auth=false with no auth collection should allow access"
    );
}
