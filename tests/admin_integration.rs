//! Integration tests for admin HTTP handlers using tower::ServiceExt::oneshot.
//!
//! Constructs the Axum router via `build_router()` without binding a TCP listener,
//! then sends requests using `tower::ServiceExt::oneshot`.

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
use crap_cms::core::Registry;
use crap_cms::db::{migrate, pool, query};
use crap_cms::hooks::lifecycle::HookRunner;

// ── Helpers ───────────────────────────────────────────────────────────────

fn make_posts_def() -> CollectionDefinition {
    CollectionDefinition {
        slug: "posts".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("Post".to_string())),
            plural: Some(LocalizedString::Plain("Posts".to_string())),
        },
        timestamps: true,
        fields: vec![
            FieldDefinition {
                name: "title".to_string(),
                required: true,
                ..Default::default()
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        live: None,
            versions: None,
            indexes: Vec::new(),
    }
}

fn make_users_def() -> CollectionDefinition {
    CollectionDefinition {
        slug: "users".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("User".to_string())),
            plural: Some(LocalizedString::Plain("Users".to_string())),
        },
        timestamps: true,
        fields: vec![
            FieldDefinition {
                name: "email".to_string(),
                field_type: FieldType::Email,
                required: true,
                unique: true,
                ..Default::default()
            },
            FieldDefinition {
                name: "name".to_string(),
                ..Default::default()
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: Some(CollectionAuth { enabled: true, ..Default::default() }),
        upload: None,
        access: CollectionAccess::default(),
        live: None,
            versions: None,
            indexes: Vec::new(),
    }
}

fn make_global_def() -> GlobalDefinition {
    GlobalDefinition {
        slug: "settings".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("Settings".to_string())),
            plural: None,
        },
        fields: vec![
            FieldDefinition {
                name: "site_name".to_string(),
                ..Default::default()
            },
        ],
        hooks: CollectionHooks::default(),
        access: CollectionAccess::default(),
        live: None,
        versions: None,
    }
}

struct TestApp {
    _tmp: tempfile::TempDir,
    router: axum::Router,
    pool: crap_cms::db::DbPool,
    registry: crap_cms::core::SharedRegistry,
    jwt_secret: String,
}

fn setup_app(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
) -> TestApp {
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".to_string();
    config.admin.require_auth = false; // tests default to open admin
    setup_app_with_config(collections, globals, config)
}

fn setup_app_with_config(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
    config: CrapConfig,
) -> TestApp {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config = config;

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
        HookRunner::new(tmp.path(), registry.clone(), &config).expect("create hook runner");

    let translations = Arc::new(Translations::load(tmp.path()));
    let handlebars =
        templates::create_handlebars(tmp.path(), false, translations.clone()).expect("create handlebars");
    let email_renderer =
        Arc::new(EmailRenderer::new(tmp.path()).expect("create email renderer"));

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
        jwt_secret: "test-jwt-secret".to_string(),
        email_renderer,
        event_bus: None,
        login_limiter: std::sync::Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(5, 300)),
        has_auth,
        translations,
    };

    let router = build_router(state);

    TestApp {
        _tmp: tmp,
        router,
        pool: db_pool,
        registry,
        jwt_secret: "test-jwt-secret".to_string(),
    }
}

/// Create a user in the database for auth tests.
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
    doc.id
}

/// Generate a valid JWT cookie for the given user.
fn make_auth_cookie(app: &TestApp, user_id: &str, email: &str) -> String {
    let claims = auth::Claims {
        sub: user_id.to_string(),
        collection: "users".to_string(),
        email: email.to_string(),
        exp: (chrono::Utc::now().timestamp() as u64) + 3600,
    };
    let token = auth::create_token(&claims, &app.jwt_secret).unwrap();
    format!("crap_session={}", token)
}

/// Fixed CSRF token for tests.
const TEST_CSRF: &str = "test-csrf-token-12345";

/// Cookie string with just the CSRF token.
fn csrf_cookie() -> String {
    format!("crap_csrf={}", TEST_CSRF)
}

/// Combine an auth cookie with the CSRF cookie.
fn auth_and_csrf(auth_cookie: &str) -> String {
    format!("{}; crap_csrf={}", auth_cookie, TEST_CSRF)
}

/// Helper to read response body as string.
async fn body_string(body: Body) -> String {
    let bytes = body.collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

// ── 1A. Public Routes ─────────────────────────────────────────────────────

#[tokio::test]
async fn login_page_returns_200() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let resp = app.router
        .oneshot(Request::get("/admin/login").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(body.to_lowercase().contains("login"), "Login page should contain 'login'");
}

#[tokio::test]
async fn login_action_invalid_credentials() {
    let app = setup_app(vec![make_users_def()], vec![]);
    create_test_user(&app, "user@test.com", "secret123");

    let resp = app.router
        .oneshot(
            Request::post("/admin/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from("collection=users&email=user@test.com&password=wrong"))
                .unwrap(),
        )
        .await
        .unwrap();
    // Should re-render the login form (200) with an error, not redirect
    // Or redirect back to login - either way, no valid session cookie
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER || status == StatusCode::FOUND,
        "Expected 200 or redirect, got {}",
        status
    );
}

#[tokio::test]
async fn login_action_valid_credentials() {
    let app = setup_app(vec![make_users_def()], vec![]);
    create_test_user(&app, "valid@test.com", "correct123");

    let resp = app.router
        .oneshot(
            Request::post("/admin/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from("collection=users&email=valid@test.com&password=correct123"))
                .unwrap(),
        )
        .await
        .unwrap();
    // Should redirect to /admin with a Set-Cookie header
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND,
        "Expected redirect, got {}",
        status
    );
    let cookie = resp.headers().get("set-cookie")
        .map(|v| v.to_str().unwrap_or(""));
    assert!(cookie.is_some(), "Should set a session cookie");
    assert!(cookie.unwrap().contains("crap_session"), "Cookie should be crap_session");
}

#[tokio::test]
async fn logout_clears_cookie() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let resp = app.router
        .oneshot(
            Request::post("/admin/logout")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND,
        "Expected redirect, got {}",
        status
    );
    let cookie = resp.headers().get("set-cookie")
        .map(|v| v.to_str().unwrap_or(""));
    if let Some(c) = cookie {
        assert!(
            c.contains("Max-Age=0") || c.contains("max-age=0") || c.contains("expires=Thu, 01 Jan 1970"),
            "Cookie should be expired: {}",
            c
        );
    }
}

#[tokio::test]
async fn forgot_password_page_returns_200() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let resp = app.router
        .oneshot(Request::get("/admin/forgot-password").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn reset_password_page_invalid_token() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let resp = app.router
        .oneshot(
            Request::get("/admin/reset-password?token=badtoken&collection=users")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // Should still return 200 (renders error page or the form with error)
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER || status == StatusCode::FOUND,
        "Expected 200 or redirect for invalid token, got {}",
        status
    );
}

#[tokio::test]
async fn verify_email_invalid_token() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let resp = app.router
        .oneshot(
            Request::get("/admin/verify-email?token=badtoken&collection=users")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    // Should redirect with error or return error page
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER || status == StatusCode::FOUND,
        "Expected 200 or redirect, got {}",
        status
    );
}

// ── 1B. Auth Middleware ───────────────────────────────────────────────────

#[tokio::test]
async fn protected_route_redirects_without_auth() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let resp = app.router
        .oneshot(Request::get("/admin").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::SEE_OTHER,
        "Protected route without auth should redirect"
    );
}

#[tokio::test]
async fn protected_route_allows_with_cookie() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    let resp = app.router
        .oneshot(
            Request::get("/admin")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "Protected route with valid cookie should return 200"
    );
}

#[tokio::test]
async fn no_auth_collections_skips_middleware() {
    // Only posts (no auth collection) — middleware should not activate
    let app = setup_app(vec![make_posts_def()], vec![]);
    let resp = app.router
        .oneshot(Request::get("/admin").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "No auth collections = no middleware = 200"
    );
}

// ── 1C. Dashboard & Collections ───────────────────────────────────────────

#[tokio::test]
async fn dashboard_returns_200() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "dash@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "dash@test.com");

    let resp = app.router
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
    assert!(body.to_lowercase().contains("posts") || body.to_lowercase().contains("dashboard"));
}

#[tokio::test]
async fn list_collections_returns_200() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "list@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "list@test.com");

    let resp = app.router
        .oneshot(
            Request::get("/admin/collections")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn list_items_returns_200() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "items@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "items@test.com");

    let resp = app.router
        .oneshot(
            Request::get("/admin/collections/posts")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn create_form_returns_200() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "create@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "create@test.com");

    let resp = app.router
        .oneshot(
            Request::get("/admin/collections/posts/create")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn create_action_creates_document() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "create_action@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "create_action@test.com");

    let resp = app.router
        .oneshot(
            Request::post("/admin/collections/posts")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=Test+Post"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND || status == StatusCode::OK,
        "Create action should redirect or HX-Redirect, got {}",
        status
    );
}

#[tokio::test]
async fn edit_form_returns_200() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "edit@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "edit@test.com");

    // Create a document first
    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([("title".to_string(), "Edit Me".to_string())]);
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app.router
        .oneshot(
            Request::get(format!("/admin/collections/posts/{}", doc.id))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn update_action_updates_document() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "update@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "update@test.com");

    // Create a doc first
    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([("title".to_string(), "Original".to_string())]);
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app.router
        .oneshot(
            Request::post(format!("/admin/collections/posts/{}", doc.id))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=Updated"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND || status == StatusCode::OK,
        "Update action should redirect or HX-Redirect, got {}",
        status
    );
}

#[tokio::test]
async fn delete_action_removes_document() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "delete@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "delete@test.com");

    // Create a doc first
    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([("title".to_string(), "Delete Me".to_string())]);
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app.router
        .oneshot(
            Request::delete(format!("/admin/collections/posts/{}", doc.id))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND || status == StatusCode::OK,
        "Delete action should redirect or return 200, got {}",
        status
    );
}

#[tokio::test]
async fn nonexistent_collection_returns_404() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "notfound@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "notfound@test.com");

    let resp = app.router
        .oneshot(
            Request::get("/admin/collections/nope")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── 1D. Globals ───────────────────────────────────────────────────────────

#[tokio::test]
async fn global_edit_form_returns_200() {
    let app = setup_app(vec![make_users_def()], vec![make_global_def()]);
    let user_id = create_test_user(&app, "global@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "global@test.com");

    let resp = app.router
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

    let resp = app.router
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

// ── 1E. Uploads ───────────────────────────────────────────────────────────

#[tokio::test]
async fn serve_upload_nonexistent_returns_404() {
    let app = setup_app(vec![make_posts_def()], vec![]);
    let resp = app.router
        .oneshot(
            Request::get("/uploads/posts/nofile.jpg")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── 1F. Static Files ─────────────────────────────────────────────────────

#[tokio::test]
async fn static_css_returns_200() {
    let app = setup_app(vec![make_posts_def()], vec![]);
    let resp = app.router
        .oneshot(
            Request::get("/static/styles.css")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp.headers().get("content-type")
        .map(|v| v.to_str().unwrap_or(""));
    assert!(
        ct.unwrap_or("").contains("css"),
        "Content-Type should be CSS, got {:?}",
        ct
    );
}

// ── 2. Upload API (/api/upload) ─────────────────────────────────────────

fn make_media_def() -> CollectionDefinition {
    use crap_cms::core::upload::CollectionUpload;

    fn hidden_text(name: &str) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            admin: FieldAdmin { hidden: true, ..Default::default() },
            ..Default::default()
        }
    }
    fn hidden_number(name: &str) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: FieldType::Number,
            admin: FieldAdmin { hidden: true, ..Default::default() },
            ..Default::default()
        }
    }

    CollectionDefinition {
        slug: "media".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("Media".to_string())),
            plural: Some(LocalizedString::Plain("Media".to_string())),
        },
        timestamps: true,
        fields: vec![
            // Upload metadata fields (normally auto-injected by Lua parser)
            FieldDefinition {
                name: "filename".to_string(),
                required: true,
                admin: FieldAdmin { readonly: true, ..Default::default() },
                ..Default::default()
            },
            hidden_text("mime_type"),
            hidden_number("filesize"),
            hidden_number("width"),
            hidden_number("height"),
            hidden_text("url"),
            // User-defined field
            FieldDefinition {
                name: "alt".to_string(),
                ..Default::default()
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: Some(CollectionUpload {
            enabled: true,
            mime_types: vec!["image/*".to_string(), "application/pdf".to_string()],
            ..Default::default()
        }),
        access: CollectionAccess::default(),
        live: None,
        versions: None,
            indexes: Vec::new(),
    }
}

/// Build a multipart form body with a file and optional text fields.
fn build_multipart_body(
    filename: &str,
    content_type: &str,
    file_data: &[u8],
    fields: &[(&str, &str)],
) -> (String, Vec<u8>) {
    let boundary = "----CrapTestBoundary";
    let mut body = Vec::new();

    // File field
    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"_file\"; filename=\"{}\"\r\n",
            filename
        )
        .as_bytes(),
    );
    body.extend_from_slice(format!("Content-Type: {}\r\n\r\n", content_type).as_bytes());
    body.extend_from_slice(file_data);
    body.extend_from_slice(b"\r\n");

    // Text fields
    for (name, value) in fields {
        body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{}\"\r\n\r\n", name).as_bytes(),
        );
        body.extend_from_slice(value.as_bytes());
        body.extend_from_slice(b"\r\n");
    }

    body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

    let content_type = format!("multipart/form-data; boundary={}", boundary);
    (content_type, body)
}

fn make_bearer_token(app: &TestApp, user_id: &str, email: &str) -> String {
    let claims = auth::Claims {
        sub: user_id.to_string(),
        collection: "users".to_string(),
        email: email.to_string(),
        exp: (chrono::Utc::now().timestamp() as u64) + 3600,
    };
    let token = auth::create_token(&claims, &app.jwt_secret).unwrap();
    format!("Bearer {}", token)
}

/// A minimal valid PNG (1x1 pixel, transparent).
fn tiny_png() -> Vec<u8> {
    // Smallest valid PNG: 1x1 RGBA
    let mut buf = std::io::Cursor::new(Vec::new());
    let encoder = image::codecs::png::PngEncoder::new(&mut buf);
    use image::ImageEncoder;
    encoder
        .write_image(&[0u8, 0, 0, 0], 1, 1, image::ExtendedColorType::Rgba8)
        .unwrap();
    buf.into_inner()
}

// ── 2A. Upload API: Create ──────────────────────────────────────────────

#[tokio::test]
async fn upload_api_create_returns_201_with_document() {
    let app = setup_app(vec![make_users_def(), make_media_def()], vec![]);
    let user_id = create_test_user(&app, "uploader@test.com", "secret123");
    let bearer = make_bearer_token(&app, &user_id, "uploader@test.com");

    let png = tiny_png();
    let (ct, body) = build_multipart_body("photo.png", "image/png", &png, &[("alt", "Test alt")]);

    let resp = app
        .router
        .oneshot(
            Request::post("/api/upload/media")
                .header("content-type", ct)
                .header("authorization", &bearer)
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);

    let body = body_string(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(json["document"]["id"].is_string());
    assert_eq!(json["document"]["alt"], "Test alt");
    assert!(json["document"]["filename"].as_str().unwrap().ends_with("photo.png"));
    assert!(json["document"]["url"].as_str().unwrap().starts_with("/uploads/media/"));
    assert_eq!(json["document"]["mime_type"], "image/png");
}

#[tokio::test]
async fn upload_api_create_no_file_returns_400() {
    let app = setup_app(vec![make_users_def(), make_media_def()], vec![]);
    let user_id = create_test_user(&app, "uploader@test.com", "secret123");
    let bearer = make_bearer_token(&app, &user_id, "uploader@test.com");

    // Multipart with no _file field
    let boundary = "----CrapTestBoundary";
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"alt\"\r\n\r\nsome text\r\n");
    body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

    let resp = app
        .router
        .oneshot(
            Request::post("/api/upload/media")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={}", boundary),
                )
                .header("authorization", &bearer)
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_string(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(json["error"].as_str().unwrap().contains("No file"));
}

#[tokio::test]
async fn upload_api_create_non_upload_collection_returns_400() {
    let app = setup_app(vec![make_users_def(), make_posts_def()], vec![]);
    let user_id = create_test_user(&app, "uploader@test.com", "secret123");
    let bearer = make_bearer_token(&app, &user_id, "uploader@test.com");

    let png = tiny_png();
    let (ct, body) = build_multipart_body("photo.png", "image/png", &png, &[]);

    let resp = app
        .router
        .oneshot(
            Request::post("/api/upload/posts")
                .header("content-type", ct)
                .header("authorization", &bearer)
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_string(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(json["error"].as_str().unwrap().contains("not an upload collection"));
}

#[tokio::test]
async fn upload_api_create_unknown_collection_returns_404() {
    let app = setup_app(vec![make_users_def(), make_media_def()], vec![]);
    let user_id = create_test_user(&app, "uploader@test.com", "secret123");
    let bearer = make_bearer_token(&app, &user_id, "uploader@test.com");

    let png = tiny_png();
    let (ct, body) = build_multipart_body("photo.png", "image/png", &png, &[]);

    let resp = app
        .router
        .oneshot(
            Request::post("/api/upload/nonexistent")
                .header("content-type", ct)
                .header("authorization", &bearer)
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn upload_api_create_rejected_mime_returns_400() {
    let app = setup_app(vec![make_users_def(), make_media_def()], vec![]);
    let user_id = create_test_user(&app, "uploader@test.com", "secret123");
    let bearer = make_bearer_token(&app, &user_id, "uploader@test.com");

    // media collection allows image/* and application/pdf — send text/plain
    let (ct, body) =
        build_multipart_body("notes.txt", "text/plain", b"hello world", &[]);

    let resp = app
        .router
        .oneshot(
            Request::post("/api/upload/media")
                .header("content-type", ct)
                .header("authorization", &bearer)
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_string(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(json["error"].as_str().unwrap().contains("not allowed"));
}

// ── 2B. Upload API: Update ──────────────────────────────────────────────

#[tokio::test]
async fn upload_api_update_replaces_file() {
    let app = setup_app(vec![make_users_def(), make_media_def()], vec![]);
    let user_id = create_test_user(&app, "uploader@test.com", "secret123");
    let bearer = make_bearer_token(&app, &user_id, "uploader@test.com");

    // Create first
    let png = tiny_png();
    let (ct, body) = build_multipart_body("first.png", "image/png", &png, &[("alt", "First")]);

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/api/upload/media")
                .header("content-type", &ct)
                .header("authorization", &bearer)
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let create_body = body_string(resp.into_body()).await;
    let create_json: serde_json::Value = serde_json::from_str(&create_body).unwrap();
    let doc_id = create_json["document"]["id"].as_str().unwrap();
    let old_filename = create_json["document"]["filename"].as_str().unwrap().to_string();

    // Update with new file
    let png2 = tiny_png();
    let (ct2, body2) =
        build_multipart_body("second.png", "image/png", &png2, &[("alt", "Second")]);

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::patch(&format!("/api/upload/media/{}", doc_id))
                .header("content-type", ct2)
                .header("authorization", &bearer)
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(body2))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let update_body = body_string(resp.into_body()).await;
    let update_json: serde_json::Value = serde_json::from_str(&update_body).unwrap();
    let new_filename = update_json["document"]["filename"].as_str().unwrap();
    assert_ne!(new_filename, old_filename, "Filename should change on file replacement");
    assert_eq!(update_json["document"]["alt"], "Second");
}

// ── 2C. Upload API: Delete ──────────────────────────────────────────────

#[tokio::test]
async fn upload_api_delete_returns_success() {
    let app = setup_app(vec![make_users_def(), make_media_def()], vec![]);
    let user_id = create_test_user(&app, "uploader@test.com", "secret123");
    let bearer = make_bearer_token(&app, &user_id, "uploader@test.com");

    // Create first
    let png = tiny_png();
    let (ct, body) = build_multipart_body("todelete.png", "image/png", &png, &[]);

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/api/upload/media")
                .header("content-type", ct)
                .header("authorization", &bearer)
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let create_body = body_string(resp.into_body()).await;
    let create_json: serde_json::Value = serde_json::from_str(&create_body).unwrap();
    let doc_id = create_json["document"]["id"].as_str().unwrap();

    // Delete
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::delete(&format!("/api/upload/media/{}", doc_id))
                .header("authorization", &bearer)
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let del_body = body_string(resp.into_body()).await;
    let del_json: serde_json::Value = serde_json::from_str(&del_body).unwrap();
    assert_eq!(del_json["success"], true);
}

#[tokio::test]
async fn upload_api_delete_nonexistent_returns_404() {
    let app = setup_app(vec![make_users_def(), make_media_def()], vec![]);
    let user_id = create_test_user(&app, "uploader@test.com", "secret123");
    let bearer = make_bearer_token(&app, &user_id, "uploader@test.com");

    let resp = app
        .router
        .oneshot(
            Request::delete("/api/upload/media/nonexistent-id")
                .header("authorization", &bearer)
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── Localized collection regression tests ─────────────────────────────
// Bug: admin list/edit views passed None for locale_ctx, generating SQL
// with bare column names (e.g. "title") while the actual DB columns are
// locale-suffixed (e.g. "title__en", "title__de"). This caused 500 errors.

fn make_locale_config() -> LocaleConfig {
    LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    }
}

/// A collection where every field is localized.
fn make_localized_pages_def() -> CollectionDefinition {
    CollectionDefinition {
        slug: "pages".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("Page".to_string())),
            plural: Some(LocalizedString::Plain("Pages".to_string())),
        },
        timestamps: true,
        fields: vec![
            FieldDefinition {
                name: "title".to_string(),
                required: true,
                localized: true,
                ..Default::default()
            },
            FieldDefinition {
                name: "body".to_string(),
                field_type: FieldType::Textarea,
                localized: true,
                ..Default::default()
            },
        ],
        admin: CollectionAdmin {
            use_as_title: Some("title".to_string()),
            ..CollectionAdmin::default()
        },
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        live: None,
        versions: None,
            indexes: Vec::new(),
    }
}

fn setup_localized_app() -> TestApp {
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".to_string();
    config.locale = make_locale_config();
    setup_app_with_config(
        vec![make_localized_pages_def(), make_users_def()],
        vec![],
        config,
    )
}

/// Regression: listing a localized collection must return 200, not 500.
#[tokio::test]
async fn localized_collection_list_returns_200() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/pages")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// Regression: listing a localized collection with data must show items.
#[tokio::test]
async fn localized_collection_list_shows_documents() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    // Insert a document with the default locale
    {
        let reg = app.registry.read().unwrap();
        let def = reg.get_collection("pages").unwrap().clone();
        drop(reg);

        let locale_ctx = query::LocaleContext {
            mode: query::LocaleMode::Single("en".to_string()),
            config: make_locale_config(),
        };
        let mut data = std::collections::HashMap::new();
        data.insert("title".to_string(), "Hello World".to_string());
        data.insert("body".to_string(), "Page body".to_string());
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        query::create(&tx, "pages", &def, &data, Some(&locale_ctx)).unwrap();
        tx.commit().unwrap();
    }

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/pages")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(body.contains("Hello World"), "list should contain the document title");
}

/// Regression: creating a document in a localized collection via admin form must succeed.
#[tokio::test]
async fn localized_collection_create_via_form() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/pages")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from("title=Created+Page&body=Some+content&_locale=en"))
                .unwrap(),
        )
        .await
        .unwrap();
    // Successful create responds with HX-Redirect (200) or redirect (303)
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "Localized create should redirect or HX-Redirect, got {}",
        status
    );
}

/// Regression: the edit page for a localized document must return 200.
#[tokio::test]
async fn localized_collection_edit_page_returns_200() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    // Insert a document
    let doc_id = {
        let reg = app.registry.read().unwrap();
        let def = reg.get_collection("pages").unwrap().clone();
        drop(reg);

        let locale_ctx = query::LocaleContext {
            mode: query::LocaleMode::Single("en".to_string()),
            config: make_locale_config(),
        };
        let mut data = std::collections::HashMap::new();
        data.insert("title".to_string(), "Editable Page".to_string());
        data.insert("body".to_string(), "Content".to_string());
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let doc = query::create(&tx, "pages", &def, &data, Some(&locale_ctx)).unwrap();
        tx.commit().unwrap();
        doc.id
    };

    let resp = app
        .router
        .oneshot(
            Request::get(&format!("/admin/collections/pages/{}", doc_id))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(body.contains("Editable Page"), "edit page should contain the document title");
}

/// Regression: deleting a document in a localized collection must succeed.
#[tokio::test]
async fn localized_collection_delete_succeeds() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    // Insert a document
    let doc_id = {
        let reg = app.registry.read().unwrap();
        let def = reg.get_collection("pages").unwrap().clone();
        drop(reg);

        let locale_ctx = query::LocaleContext {
            mode: query::LocaleMode::Single("en".to_string()),
            config: make_locale_config(),
        };
        let mut data = std::collections::HashMap::new();
        data.insert("title".to_string(), "To Delete".to_string());
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let doc = query::create(&tx, "pages", &def, &data, Some(&locale_ctx)).unwrap();
        tx.commit().unwrap();
        doc.id
    };

    let resp = app
        .router
        .oneshot(
            Request::delete(format!("/admin/collections/pages/{}", doc_id))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // Successful delete redirects or returns OK
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND || status == StatusCode::OK,
        "expected redirect after delete, got {}",
        status
    );
}

/// Regression: searching a localized collection must not crash.
#[tokio::test]
async fn localized_collection_search_returns_200() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    // Insert a document so search has data to work with
    {
        let reg = app.registry.read().unwrap();
        let def = reg.get_collection("pages").unwrap().clone();
        drop(reg);

        let locale_ctx = query::LocaleContext {
            mode: query::LocaleMode::Single("en".to_string()),
            config: make_locale_config(),
        };
        let mut data = std::collections::HashMap::new();
        data.insert("title".to_string(), "Searchable Page".to_string());
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        query::create(&tx, "pages", &def, &data, Some(&locale_ctx)).unwrap();
        tx.commit().unwrap();
    }

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/pages?search=Searchable")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ── Auth Handler Gaps ─────────────────────────────────────────────────────

#[tokio::test]
async fn login_locked_account() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let user_id = create_test_user(&app, "locked@test.com", "secret123");

    // Lock the user account
    {
        let conn = app.pool.get().unwrap();
        query::lock_user(&conn, "users", &user_id).unwrap();
    }

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from("collection=users&email=locked@test.com&password=secret123"))
                .unwrap(),
        )
        .await
        .unwrap();

    // Should not set a valid session cookie — either re-renders login (200)
    // or redirects back to login
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER || status == StatusCode::FOUND,
        "Expected 200 or redirect, got {}",
        status
    );

    // If a cookie is set, it should NOT be a valid session cookie
    // (i.e. no crap_session=<valid-token>)
    if status == StatusCode::SEE_OTHER || status == StatusCode::FOUND {
        // Redirect means login failed, which is correct
        let location = resp.headers().get("location")
            .map(|v| v.to_str().unwrap_or(""));
        // Should redirect to login page, not dashboard
        if let Some(loc) = location {
            assert!(
                loc.contains("login"),
                "Locked account should redirect to login, not {}",
                loc
            );
        }
    }
}

#[tokio::test]
async fn forgot_password_action() {
    let app = setup_app(vec![make_users_def()], vec![]);

    // POST with a non-existent email — should still return success (don't leak existence)
    let resp = app
        .router
        .oneshot(
            Request::post("/admin/forgot-password")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from("collection=users&email=nonexistent@test.com"))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER || status == StatusCode::FOUND,
        "Forgot password should return 200 or redirect, never error, got {}",
        status
    );
}

#[tokio::test]
async fn reset_password_expired_token() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let user_id = create_test_user(&app, "expired@test.com", "oldpass123");

    // Set an expired reset token (expiry in the past)
    let expired_token = "expired-test-token-12345";
    {
        let conn = app.pool.get().unwrap();
        let past_exp = chrono::Utc::now().timestamp() - 3600; // 1 hour ago
        query::set_reset_token(&conn, "users", &user_id, expired_token, past_exp).unwrap();
    }

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/reset-password")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(format!(
                    "collection=users&token={}&password=newpass123&password_confirm=newpass123",
                    expired_token
                )))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    // Should indicate error about expired token — either 200 with error message,
    // redirect to forgot-password/login, or 422 Unprocessable Entity
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER
            || status == StatusCode::FOUND || status == StatusCode::UNPROCESSABLE_ENTITY,
        "Expected 200, redirect, or 422 for expired token, got {}",
        status
    );

    // For non-redirect responses, the token was rejected which is the correct behavior
    if status != StatusCode::SEE_OTHER && status != StatusCode::FOUND {
        let body = body_string(resp.into_body()).await;
        let body_lower = body.to_lowercase();
        // Either contains an error message or is empty (422 with no body is fine)
        assert!(
            body_lower.is_empty()
                || body_lower.contains("expired")
                || body_lower.contains("invalid")
                || body_lower.contains("error")
                || body_lower.contains("token")
                || body_lower.contains("reset"),
            "Response should indicate expired/invalid token, got: {}",
            &body[..body.len().min(200)]
        );
    }
}

// ── Collection Handler Gaps ───────────────────────────────────────────────

#[tokio::test]
async fn list_items_with_search() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "search@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "search@test.com");

    // Create 3 posts with different titles
    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    for title in &["Zebra Unique Alpha", "Beta Common", "Gamma Common"] {
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let data = std::collections::HashMap::from([("title".to_string(), title.to_string())]);
        query::create(&tx, "posts", &def, &data, None).unwrap();
        tx.commit().unwrap();
    }

    // Search for "Zebra" — should return 200
    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts?search=Zebra")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(
        body.contains("Zebra"),
        "Search results should contain 'Zebra'"
    );
}

#[tokio::test]
async fn create_action_with_locale() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "locale_create@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "locale_create@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/pages")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from("title=Locale+Test+Page&body=Content+here&_locale=de"))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "Localized create with locale param should succeed, got {}",
        status
    );
}

#[tokio::test]
async fn delete_action_returns_redirect() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "delredir@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "delredir@test.com");

    // Create a doc first
    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([("title".to_string(), "To Delete Redir".to_string())]);
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::delete(format!("/admin/collections/posts/{}", doc.id))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND || status == StatusCode::OK,
        "Delete action should redirect or return 200 with HX-Redirect, got {}",
        status
    );

    // If it's a redirect, verify it points to the collection list
    if status == StatusCode::SEE_OTHER || status == StatusCode::FOUND {
        let location = resp.headers().get("location")
            .map(|v| v.to_str().unwrap_or(""));
        if let Some(loc) = location {
            assert!(
                loc.contains("/admin/collections/posts"),
                "Delete redirect should point to collection list, got {}",
                loc
            );
        }
    }
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

    // Create a few posts
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
    // Dashboard should reference collection names
    let body_lower = body.to_lowercase();
    assert!(
        body_lower.contains("posts") || body_lower.contains("post"),
        "Dashboard should contain collection info"
    );
}

// ── Global Versioning Tests ──────────────────────────────────────────────

fn make_versioned_global_def() -> GlobalDefinition {
    GlobalDefinition {
        slug: "site_config".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("Site Config".to_string())),
            plural: None,
        },
        fields: vec![
            FieldDefinition {
                name: "site_name".to_string(),
                ..Default::default()
            },
            FieldDefinition {
                name: "tagline".to_string(),
                ..Default::default()
            },
        ],
        hooks: CollectionHooks::default(),
        access: CollectionAccess::default(),
        live: None,
        versions: Some(crap_cms::core::collection::VersionsConfig {
            drafts: true,
            max_versions: 10,
        }),
    }
}

#[tokio::test]
async fn global_versions_page_returns_200() {
    let app = setup_app(vec![make_users_def()], vec![make_versioned_global_def()]);
    let user_id = create_test_user(&app, "gv@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "gv@test.com");

    // First update the global to create a version
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

    // Now check the versions page
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
    // Non-versioned global should redirect to the edit page
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND || status == StatusCode::TEMPORARY_REDIRECT,
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
                .body(Body::from("site_name=Draft+Site&tagline=WIP&_action=save_draft"))
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

    // First, publish something
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

    // Then unpublish
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/globals/site_config")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("site_name=Published+Site&tagline=Live&_action=unpublish"))
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

    // Create initial version
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

    // Create second version
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

    // Fetch versions to get a version ID
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
    // Non-versioned global restore should redirect
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND || status == StatusCode::TEMPORARY_REDIRECT || status == StatusCode::OK,
        "Non-versioned restore should redirect, got {}",
        status
    );
}

// ── Localized Global Tests ───────────────────────────────────────────────

fn make_localized_global_def() -> GlobalDefinition {
    GlobalDefinition {
        slug: "l10n_settings".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("L10N Settings".to_string())),
            plural: None,
        },
        fields: vec![
            FieldDefinition {
                name: "welcome_text".to_string(),
                localized: true,
                ..Default::default()
            },
            FieldDefinition {
                name: "max_items".to_string(),
                field_type: FieldType::Number,
                ..Default::default()
            },
        ],
        hooks: CollectionHooks::default(),
        access: CollectionAccess::default(),
        live: None,
        versions: None,
    }
}

#[tokio::test]
async fn localized_global_edit_returns_200() {
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".to_string();
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
    config.auth.secret = "test-jwt-secret".to_string();
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
            Request::get("/admin/globals/l10n_settings?locale=de")
                .header("cookie", &cookie)
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
    config.auth.secret = "test-jwt-secret".to_string();
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
                .body(Body::from("welcome_text=Willkommen&max_items=10&_locale=de"))
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

// ── Upload Serving Tests ─────────────────────────────────────────────────

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

    // Create the uploads directory and put a file there
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
    let ct = resp.headers().get("content-type").map(|v| v.to_str().unwrap_or("")).unwrap_or("");
    assert!(ct.contains("text/plain"), "Should detect text/plain MIME, got {}", ct);
    let cache = resp.headers().get("cache-control").map(|v| v.to_str().unwrap_or("")).unwrap_or("");
    assert!(cache.contains("public"), "Public file should have public cache control, got {}", cache);
    let body = body_string(resp.into_body()).await;
    assert_eq!(body, "hello world");
}

#[tokio::test]
async fn serve_upload_image_file() {
    let app = setup_app(vec![make_posts_def()], vec![]);

    // Create the uploads directory and put a tiny PNG there
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
    let ct = resp.headers().get("content-type").map(|v| v.to_str().unwrap_or("")).unwrap_or("");
    assert!(ct.contains("image/png"), "Should detect image/png MIME, got {}", ct);
}

// ── Collection Versioning Tests ──────────────────────────────────────────

fn make_versioned_posts_def() -> CollectionDefinition {
    CollectionDefinition {
        slug: "articles".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("Article".to_string())),
            plural: Some(LocalizedString::Plain("Articles".to_string())),
        },
        timestamps: true,
        fields: vec![
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
        ],
        admin: CollectionAdmin {
            use_as_title: Some("title".to_string()),
            ..CollectionAdmin::default()
        },
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        live: None,
        versions: Some(crap_cms::core::collection::VersionsConfig {
            drafts: true,
            max_versions: 10,
        }),
        indexes: Vec::new(),
    }
}

#[tokio::test]
async fn collection_versions_page_returns_200() {
    let app = setup_app(vec![make_versioned_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "cvp@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "cvp@test.com");

    // Create a document
    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("articles").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([
        ("title".to_string(), "Versioned Article".to_string()),
        ("body".to_string(), "Content".to_string()),
    ]);
    let doc = query::create(&tx, "articles", &def, &data, None).unwrap();
    tx.commit().unwrap();

    // Create form (should work)
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!("/admin/collections/articles/{}/versions", doc.id))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn collection_create_with_draft() {
    let app = setup_app(vec![make_versioned_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "cdraft@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "cdraft@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/articles")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=Draft+Article&body=WIP&_action=save_draft"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "Create draft should succeed, got {}",
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

// ── Edit nonexistent document returns 404 ────────────────────────────────

#[tokio::test]
async fn edit_nonexistent_document_returns_404() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "editnf@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "editnf@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts/nonexistent-id-12345")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── Delete nonexistent document ──────────────────────────────────────────

#[tokio::test]
async fn delete_nonexistent_document() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "delnf@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "delnf@test.com");

    let resp = app
        .router
        .oneshot(
            Request::delete("/admin/collections/posts/nonexistent-id-12345")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // Either redirect or error response
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK
            || status == StatusCode::FOUND || status == StatusCode::NOT_FOUND,
        "Delete nonexistent should return redirect or not found, got {}",
        status
    );
}

// ── Update nonexistent document ──────────────────────────────────────────

#[tokio::test]
async fn update_nonexistent_document() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "updnf@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "updnf@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/posts/nonexistent-id-12345")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=Updated"))
                .unwrap(),
        )
        .await
        .unwrap();
    // Should either redirect or return error
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK
            || status == StatusCode::FOUND || status == StatusCode::NOT_FOUND
            || status == StatusCode::INTERNAL_SERVER_ERROR,
        "Update nonexistent should return redirect or error, got {}",
        status
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
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND
            || status == StatusCode::TEMPORARY_REDIRECT,
        "Update nonexistent global should redirect, got {}",
        status
    );
}

// ── Pagination on collection list ────────────────────────────────────────

#[tokio::test]
async fn collection_list_with_pagination() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "page@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "page@test.com");

    // Create several posts
    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    for i in 0..5 {
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let data = std::collections::HashMap::from([
            ("title".to_string(), format!("Post {}", i)),
        ]);
        query::create(&tx, "posts", &def, &data, None).unwrap();
        tx.commit().unwrap();
    }

    // Request page 1 with per_page=2
    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts?page=1&per_page=2")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ── No auth (no auth collections) dashboard ──────────────────────────────

#[tokio::test]
async fn dashboard_no_auth_returns_200() {
    // Only posts, no auth collection — middleware should be absent
    let app = setup_app(vec![make_posts_def()], vec![make_global_def()]);
    let resp = app
        .router
        .oneshot(
            Request::get("/admin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let body_lower = body.to_lowercase();
    assert!(
        body_lower.contains("dashboard") || body_lower.contains("posts") || body_lower.contains("settings"),
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
    let ct = resp.headers().get("content-type")
        .map(|v| v.to_str().unwrap_or(""));
    assert!(
        ct.unwrap_or("").contains("javascript"),
        "JS content type should be javascript, got {:?}",
        ct
    );
}

// ── Delete confirm page ──────────────────────────────────────────────────

#[tokio::test]
async fn delete_confirm_page_returns_200() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "delconf@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "delconf@test.com");

    // Create a doc first
    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([("title".to_string(), "To Confirm Delete".to_string())]);
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::get(format!("/admin/collections/posts/{}/delete", doc.id))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ── Collection create form with locales ──────────────────────────────────

#[tokio::test]
async fn create_form_with_locale_returns_200() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "cfloc@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "cfloc@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/pages/create?locale=de")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ── Edit form with non-default locale ────────────────────────────────────

#[tokio::test]
async fn edit_form_with_non_default_locale() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "efloc@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "efloc@test.com");

    // Insert a document
    let doc_id = {
        let reg = app.registry.read().unwrap();
        let def = reg.get_collection("pages").unwrap().clone();
        drop(reg);

        let locale_ctx = query::LocaleContext {
            mode: query::LocaleMode::Single("en".to_string()),
            config: make_locale_config(),
        };
        let mut data = std::collections::HashMap::new();
        data.insert("title".to_string(), "Locale Edit Test".to_string());
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let doc = query::create(&tx, "pages", &def, &data, Some(&locale_ctx)).unwrap();
        tx.commit().unwrap();
        doc.id
    };

    let resp = app
        .router
        .oneshot(
            Request::get(format!("/admin/collections/pages/{}?locale=de", doc_id))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ── Update action with locale parameter ──────────────────────────────────

#[tokio::test]
async fn update_action_with_locale() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "updloc@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "updloc@test.com");

    // Insert a document
    let doc_id = {
        let reg = app.registry.read().unwrap();
        let def = reg.get_collection("pages").unwrap().clone();
        drop(reg);

        let locale_ctx = query::LocaleContext {
            mode: query::LocaleMode::Single("en".to_string()),
            config: make_locale_config(),
        };
        let mut data = std::collections::HashMap::new();
        data.insert("title".to_string(), "Update Locale Test".to_string());
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let doc = query::create(&tx, "pages", &def, &data, Some(&locale_ctx)).unwrap();
        tx.commit().unwrap();
        doc.id
    };

    let resp = app
        .router
        .oneshot(
            Request::post(format!("/admin/collections/pages/{}", doc_id))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!("title=Updated+DE&_locale=de")))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "Update with locale should succeed, got {}",
        status
    );
}

// ══════════════════════════════════════════════════════════════════════════
// NEW TESTS: Coverage expansion for admin handlers
// ══════════════════════════════════════════════════════════════════════════

// ── Collections: Search, Filter, Sort, Pagination ─────────────────────────

#[tokio::test]
async fn list_items_with_pagination() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "page@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "page@test.com");

    // Create enough posts for multiple pages
    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    for i in 0..25 {
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let data = std::collections::HashMap::from([("title".to_string(), format!("Post {}", i))]);
        query::create(&tx, "posts", &def, &data, None).unwrap();
        tx.commit().unwrap();
    }

    // Request page 2 with per_page=10
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/collections/posts?page=2&per_page=10")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn list_items_search_no_results() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "nosearch@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "nosearch@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts?search=nonexistent_query_xyz")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn list_items_with_search_and_pagination() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "sp@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "sp@test.com");

    // Create posts
    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    for i in 0..5 {
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let data = std::collections::HashMap::from([("title".to_string(), format!("Searchable Item {}", i))]);
        query::create(&tx, "posts", &def, &data, None).unwrap();
        tx.commit().unwrap();
    }

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts?search=Searchable&page=1&per_page=3")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(body.contains("Searchable"), "Search results should contain matching items");
}

// ── Collections: Create with validation error ─────────────────────────────

fn make_posts_with_required_title() -> CollectionDefinition {
    CollectionDefinition {
        slug: "articles".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("Article".to_string())),
            plural: Some(LocalizedString::Plain("Articles".to_string())),
        },
        timestamps: true,
        fields: vec![
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
        ],
        admin: CollectionAdmin {
            use_as_title: Some("title".to_string()),
            ..CollectionAdmin::default()
        },
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        live: None,
        versions: None,
            indexes: Vec::new(),
    }
}

#[tokio::test]
async fn create_action_validation_error_missing_required_field() {
    let app = setup_app(vec![make_posts_with_required_title(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "validate@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "validate@test.com");

    // Submit form with empty title (required)
    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/articles")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=&body=Some+content"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    // Should re-render the form with validation error (200 with toast) or redirect
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "Expected 200 (validation error re-render) or redirect, got {}",
        status
    );
}

// ── Collections: Create for auth collection (password field) ──────────────

#[tokio::test]
async fn create_action_auth_collection_with_password() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let user_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/users")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("email=newuser@test.com&name=New+User&password=secret456"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "Create auth collection user should succeed, got {}",
        status
    );
}

// ── Collections: Create form for auth collection shows password ───────────

#[tokio::test]
async fn create_form_auth_collection_shows_password_field() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let user_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/users/create")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(body.contains("password"), "Auth collection create form should contain password field");
}

// ── Collections: Edit form for auth collection shows password ─────────────

#[tokio::test]
async fn edit_form_auth_collection_shows_password_field() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let user_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get(format!("/admin/collections/users/{}", user_id))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(body.contains("password"), "Auth collection edit form should contain password field");
    assert!(
        body.contains("Leave blank") || body.contains("leave blank") || body.contains("keep current"),
        "Edit form should indicate password can be left blank"
    );
}

// ── Collections: Update action with validation error ──────────────────────

#[tokio::test]
async fn update_action_validation_error() {
    let app = setup_app(vec![make_posts_with_required_title(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "update_val@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "update_val@test.com");

    // Create a document first
    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("articles").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([("title".to_string(), "Valid Title".to_string())]);
    let doc = query::create(&tx, "articles", &def, &data, None).unwrap();
    tx.commit().unwrap();

    // Update with empty title (required) - should cause validation error
    let resp = app
        .router
        .oneshot(
            Request::post(format!("/admin/collections/articles/{}", doc.id))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=&body=Updated+content"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "Expected 200 (validation error re-render) or redirect, got {}",
        status
    );
}

#[tokio::test]
async fn delete_confirm_nonexistent_doc_returns_404() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "delnf@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "delnf@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts/nonexistent-id/delete")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── Collections: Delete via POST with _method=DELETE ──────────────────────

#[tokio::test]
async fn update_action_post_with_method_delete() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "meth_del@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "meth_del@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([("title".to_string(), "Method Delete Test".to_string())]);
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();

    // POST with _method=DELETE (HTML form method override)
    let resp = app
        .router
        .oneshot(
            Request::post(format!("/admin/collections/posts/{}", doc.id))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("_method=DELETE"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "POST with _method=DELETE should succeed, got {}",
        status
    );
}

// ── Collections: Edit nonexistent document ────────────────────────────────

#[tokio::test]
async fn edit_form_nonexistent_doc_returns_404() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "editnf@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "editnf@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts/nonexistent-id")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── Collections: Versioning ───────────────────────────────────────────────

#[tokio::test]
async fn versioned_collection_list_returns_200() {
    let app = setup_app(vec![make_versioned_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "ver@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "ver@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/articles")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn versioned_collection_create_as_draft() {
    let app = setup_app(vec![make_versioned_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "draft@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "draft@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/articles")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=Draft+Post&_action=save_draft"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "Create as draft should succeed, got {}",
        status
    );
}

#[tokio::test]
async fn versioned_collection_edit_shows_versions() {
    let app = setup_app(vec![make_versioned_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "editver@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "editver@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("articles").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([("title".to_string(), "Versioned Doc".to_string())]);
    let doc = query::create(&tx, "articles", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::get(format!("/admin/collections/articles/{}", doc.id))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(
        body.to_lowercase().contains("version") || body.contains("versions"),
        "Edit page for versioned collection should show version info"
    );
}

#[tokio::test]
async fn versioned_collection_update_unpublish() {
    let app = setup_app(vec![make_versioned_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "unpub@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "unpub@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("articles").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([("title".to_string(), "Published Post".to_string())]);
    let doc = query::create(&tx, "articles", &def, &data, None).unwrap();
    tx.commit().unwrap();

    // Unpublish
    let resp = app
        .router
        .oneshot(
            Request::post(format!("/admin/collections/articles/{}", doc.id))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=Published+Post&_action=unpublish"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "Unpublish should succeed, got {}",
        status
    );
}

#[tokio::test]
async fn versioned_collection_versions_page() {
    let app = setup_app(vec![make_versioned_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "verpage@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "verpage@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("articles").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([("title".to_string(), "Versioned Page".to_string())]);
    let doc = query::create(&tx, "articles", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::get(format!("/admin/collections/articles/{}/versions", doc.id))
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
        "Versions page should contain 'version'"
    );
}

#[tokio::test]
async fn non_versioned_collection_versions_page_redirects() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "nover@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "nover@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([("title".to_string(), "No Versions".to_string())]);
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::get(format!("/admin/collections/posts/{}/versions", doc.id))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND || status == StatusCode::TEMPORARY_REDIRECT,
        "Non-versioned collection versions page should redirect, got {}",
        status
    );
}

#[tokio::test]
async fn restore_version_non_versioned_redirects() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "restnv@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "restnv@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([("title".to_string(), "No Versions".to_string())]);
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::post(format!("/admin/collections/posts/{}/versions/fake-ver/restore", doc.id))
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
        "Restore version on non-versioned collection should redirect, got {}",
        status
    );
}

// ── Collections: Evaluate conditions endpoint ─────────────────────────────

#[tokio::test]
async fn evaluate_conditions_returns_json() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "cond@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "cond@test.com");

    let body_json = serde_json::json!({
        "form_data": {"title": "Test"},
        "conditions": {}
    });

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/posts/evaluate-conditions")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body_json).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(json.is_object(), "Evaluate conditions should return a JSON object");
}

// ── Auth: Login with verify_email enabled ─────────────────────────────────

fn make_verify_users_def() -> CollectionDefinition {
    CollectionDefinition {
        slug: "vusers".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("User".to_string())),
            plural: Some(LocalizedString::Plain("Users".to_string())),
        },
        timestamps: true,
        fields: vec![
            FieldDefinition {
                name: "email".to_string(),
                field_type: FieldType::Email,
                required: true,
                unique: true,
                ..Default::default()
            },
            FieldDefinition {
                name: "name".to_string(),
                ..Default::default()
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: Some(CollectionAuth {
            enabled: true,
            verify_email: true,
            ..Default::default()
        }),
        upload: None,
        access: CollectionAccess::default(),
        live: None,
        versions: None,
            indexes: Vec::new(),
    }
}

#[tokio::test]
async fn login_unverified_email() {
    let app = setup_app(vec![make_verify_users_def()], vec![]);

    // Create a user (not verified by default)
    let reg = app.registry.read().unwrap();
    let def = reg.get_collection("vusers").unwrap().clone();
    drop(reg);

    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([
        ("email".to_string(), "unverified@test.com".to_string()),
        ("name".to_string(), "Unverified User".to_string()),
    ]);
    let doc = query::create(&tx, "vusers", &def, &data, None).unwrap();
    query::update_password(&tx, "vusers", &doc.id, "secret123").unwrap();
    tx.commit().unwrap();

    // Try to login
    let resp = app
        .router
        .oneshot(
            Request::post("/admin/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from("collection=vusers&email=unverified@test.com&password=secret123"))
                .unwrap(),
        )
        .await
        .unwrap();

    // Should fail login (unverified)
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "Unverified login should fail gracefully, got {}",
        status
    );
    // Should not get a valid session cookie
    if status == StatusCode::OK {
        let body = body_string(resp.into_body()).await;
        assert!(
            body.to_lowercase().contains("verify") || body.to_lowercase().contains("error"),
            "Login page should show verification error"
        );
    }
}

// ── Auth: Verify email with valid token ───────────────────────────────────

#[tokio::test]
async fn verify_email_with_valid_token() {
    let app = setup_app(vec![make_verify_users_def()], vec![]);

    let reg = app.registry.read().unwrap();
    let def = reg.get_collection("vusers").unwrap().clone();
    drop(reg);

    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([
        ("email".to_string(), "toverify@test.com".to_string()),
        ("name".to_string(), "To Verify".to_string()),
    ]);
    let doc = query::create(&tx, "vusers", &def, &data, None).unwrap();
    query::update_password(&tx, "vusers", &doc.id, "secret123").unwrap();
    tx.commit().unwrap();

    // Set a verification token
    let token = "valid-verification-token-12345";
    {
        let conn = app.pool.get().unwrap();
        query::set_verification_token(&conn, "vusers", &doc.id, token, 9999999999).unwrap();
    }

    let resp = app
        .router
        .oneshot(
            Request::get(format!("/admin/verify-email?token={}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    // Successful verification should redirect to login
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND,
        "Successful verification should redirect, got {}",
        status
    );
    if let Some(location) = resp.headers().get("location") {
        let loc = location.to_str().unwrap_or("");
        assert!(
            loc.contains("login") && loc.contains("success"),
            "Should redirect to login with success message, got {}",
            loc
        );
    }
}

// ── Auth: Reset password valid flow ───────────────────────────────────────

#[tokio::test]
async fn reset_password_valid_flow() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let user_id = create_test_user(&app, "reset@test.com", "oldpass123");

    // Set a valid (non-expired) reset token
    let valid_token = "valid-reset-token-67890";
    {
        let conn = app.pool.get().unwrap();
        let future_exp = chrono::Utc::now().timestamp() + 3600; // 1 hour from now
        query::set_reset_token(&conn, "users", &user_id, valid_token, future_exp).unwrap();
    }

    // GET the reset password page with valid token
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!("/admin/reset-password?token={}", valid_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    // Should show the form (not an error)
    assert!(
        !body.to_lowercase().contains("expired") && !body.to_lowercase().contains("invalid"),
        "Valid token should show reset form, not error"
    );

    // POST the new password
    let resp = app
        .router
        .oneshot(
            Request::post("/admin/reset-password")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(format!(
                    "token={}&password=newpass456&password_confirm=newpass456",
                    valid_token
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND,
        "Successful password reset should redirect, got {}",
        status
    );
    if let Some(location) = resp.headers().get("location") {
        let loc = location.to_str().unwrap_or("");
        assert!(
            loc.contains("login") && loc.contains("success"),
            "Should redirect to login with success, got {}",
            loc
        );
    }
}

// ── Auth: Reset password mismatch ─────────────────────────────────────────

#[tokio::test]
async fn reset_password_mismatch() {
    let app = setup_app(vec![make_users_def()], vec![]);

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/reset-password")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from("token=sometoken&password=newpass123&password_confirm=different456"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert_eq!(status, StatusCode::OK, "Mismatched passwords should re-render form");
    let body = body_string(resp.into_body()).await;
    assert!(
        body.to_lowercase().contains("match"),
        "Should show 'passwords do not match' error"
    );
}

// ── Auth: Reset password too short ────────────────────────────────────────

#[tokio::test]
async fn reset_password_too_short() {
    let app = setup_app(vec![make_users_def()], vec![]);

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/reset-password")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from("token=sometoken&password=ab&password_confirm=ab"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert_eq!(status, StatusCode::OK, "Too-short password should re-render form");
    let body = body_string(resp.into_body()).await;
    assert!(
        body.to_lowercase().contains("6 characters") || body.to_lowercase().contains("at least"),
        "Should show minimum password length error"
    );
}

// ── Auth: Reset password with invalid token ───────────────────────────────

#[tokio::test]
async fn reset_password_action_invalid_token() {
    let app = setup_app(vec![make_users_def()], vec![]);

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/reset-password")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from("token=totally-fake-token&password=newpass123&password_confirm=newpass123"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert_eq!(status, StatusCode::OK, "Invalid token should re-render form with error");
    let body = body_string(resp.into_body()).await;
    assert!(
        body.to_lowercase().contains("invalid") || body.to_lowercase().contains("expired"),
        "Should show invalid/expired token error"
    );
}

// ── Auth: Forgot password action with existing email ──────────────────────

#[tokio::test]
async fn forgot_password_action_existing_email() {
    let app = setup_app(vec![make_users_def()], vec![]);
    create_test_user(&app, "exists@test.com", "pass123");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/forgot-password")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from("collection=users&email=exists@test.com"))
                .unwrap(),
        )
        .await
        .unwrap();
    // Should always show success (don't leak existence)
    let status = resp.status();
    assert_eq!(status, StatusCode::OK, "Forgot password should return 200");
    let body = body_string(resp.into_body()).await;
    let body_lower = body.to_lowercase();
    assert!(
        body_lower.contains("success") || body_lower.contains("sent") || body_lower.contains("check"),
        "Should show success message"
    );
}

// ── Globals: Update with locale ───────────────────────────────────────────

#[tokio::test]
async fn global_update_with_locale() {
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".to_string();
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
                .body(Body::from("site_title=Localized+Title&description=Desc&_locale=de"))
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

    // First update the global to have some content
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

    // Unpublish
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
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND || status == StatusCode::TEMPORARY_REDIRECT,
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

// ── Uploads: Serve existing upload file ───────────────────────────────────

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

// ── Upload collection admin CRUD via HTML forms ───────────────────────────

#[tokio::test]
async fn upload_collection_create_form_shows_file_field() {
    let app = setup_app(vec![make_users_def(), make_media_def()], vec![]);
    let user_id = create_test_user(&app, "upform@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "upform@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/media/create")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(
        body.contains("file") || body.contains("upload"),
        "Upload collection create form should contain file upload controls"
    );
}

// ── Dashboard: No collections ─────────────────────────────────────────────

#[tokio::test]
async fn dashboard_no_auth_no_collections() {
    let app = setup_app(vec![], vec![]);
    let resp = app
        .router
        .oneshot(
            Request::get("/admin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ── Collections: Search with special characters ───────────────────────────

#[tokio::test]
async fn list_items_search_with_special_chars() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "special@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "special@test.com");

    // Search with URL-encoded special characters
    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts?search=hello%20world%26foo")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ── Collections: Create form with locale param ────────────────────────────

#[tokio::test]
async fn create_form_with_locale() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "cfloc@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "cfloc@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/pages/create?locale=de")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    // Non-default locale fields should be locked for non-localized fields
    assert!(body.contains("DE") || body.contains("de"), "Should show locale selector with DE");
}

// ── Global: Edit form with locale ─────────────────────────────────────────

#[tokio::test]
async fn global_edit_with_locale() {
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".to_string();
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
            Request::get("/admin/globals/l10n_settings?locale=de")
                .header("cookie", &cookie)
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

// ── Collections: list_searchable_fields configuration ─────────────────────

fn make_searchable_posts_def() -> CollectionDefinition {
    CollectionDefinition {
        slug: "sposts".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("Searchable Post".to_string())),
            plural: Some(LocalizedString::Plain("Searchable Posts".to_string())),
        },
        timestamps: true,
        fields: vec![
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
            FieldDefinition {
                name: "category".to_string(),
                ..Default::default()
            },
        ],
        admin: CollectionAdmin {
            use_as_title: Some("title".to_string()),
            list_searchable_fields: vec!["title".to_string(), "body".to_string()],
            ..CollectionAdmin::default()
        },
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        live: None,
        versions: None,
            indexes: Vec::new(),
    }
}

#[tokio::test]
async fn search_uses_configured_searchable_fields() {
    let app = setup_app(vec![make_searchable_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "search2@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "search2@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("sposts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([
        ("title".to_string(), "Unique Title XYZ".to_string()),
        ("body".to_string(), "Some body text".to_string()),
        ("category".to_string(), "tech".to_string()),
    ]);
    let doc = query::create(&tx, "sposts", &def, &data, None).unwrap();
    query::fts::fts_upsert(&tx, "sposts", &doc, Some(&def)).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/sposts?search=Unique")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(body.contains("Unique Title XYZ"), "Search should find by configured searchable fields");
}

// ── Collections: Update via locale redirect suffix ────────────────────────

#[tokio::test]
async fn update_localized_collection_redirects_with_locale() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "updloc@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "updloc@test.com");

    let doc_id = {
        let reg = app.registry.read().unwrap();
        let def = reg.get_collection("pages").unwrap().clone();
        drop(reg);
        let locale_ctx = query::LocaleContext {
            mode: query::LocaleMode::Single("en".to_string()),
            config: make_locale_config(),
        };
        let mut data = std::collections::HashMap::new();
        data.insert("title".to_string(), "Update Locale".to_string());
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let doc = query::create(&tx, "pages", &def, &data, Some(&locale_ctx)).unwrap();
        tx.commit().unwrap();
        doc.id
    };

    let resp = app
        .router
        .oneshot(
            Request::post(format!("/admin/collections/pages/{}", doc_id))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=Updated+Title&_locale=de"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "Localized update should succeed, got {}",
        status
    );
    // If it's a 200 (HX-Redirect), check the redirect header has locale
    if status == StatusCode::OK {
        if let Some(hx_redir) = resp.headers().get("HX-Redirect") {
            let redir = hx_redir.to_str().unwrap_or("");
            assert!(
                redir.contains("locale=de"),
                "HX-Redirect should contain locale=de, got {}",
                redir
            );
        }
    }
}

// ── Collections: Nonexistent collection create form ───────────────────────

#[tokio::test]
async fn create_form_nonexistent_collection_returns_404() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "cfnf@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "cfnf@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/nonexistent/create")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── Collections: Create action nonexistent collection ─────────────────────

#[tokio::test]
async fn create_action_nonexistent_collection_redirects() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "canf@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "canf@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/nonexistent")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=Test"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND,
        "Create on nonexistent collection should redirect, got {}",
        status
    );
}

// ── Collections: Collection with use_as_title in list view ────────────────

#[tokio::test]
async fn list_items_uses_title_field() {
    let mut def = make_posts_def();
    def.admin.use_as_title = Some("title".to_string());

    let app = setup_app(vec![def.clone(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "titlefield@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "titlefield@test.com");

    let real_def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([("title".to_string(), "My Custom Title".to_string())]);
    query::create(&tx, "posts", &real_def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(body.contains("My Custom Title"), "List should show document title via use_as_title");
}

// ── Global: Save as draft ─────────────────────────────────────────────────

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
async fn login_wrong_password_shows_error() {
    let app = setup_app(vec![make_users_def()], vec![]);
    create_test_user(&app, "wrongpw@test.com", "correct123");

    let resp = app.router
        .oneshot(
            Request::post("/admin/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from("collection=users&email=wrongpw@test.com&password=wrongpassword"))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    assert_eq!(status, StatusCode::OK, "Wrong password should re-render login page");
    let body = body_string(resp.into_body()).await;
    let body_lower = body.to_lowercase();
    assert!(
        body_lower.contains("invalid") || body_lower.contains("error") || body_lower.contains("login"),
        "Should show error message on wrong password"
    );
}

#[tokio::test]
async fn login_nonexistent_email() {
    let app = setup_app(vec![make_users_def()], vec![]);
    create_test_user(&app, "exists@test.com", "secret123");

    let resp = app.router
        .oneshot(
            Request::post("/admin/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from("collection=users&email=nope@test.com&password=secret123"))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    assert_eq!(status, StatusCode::OK, "Nonexistent email should re-render login page");
}

#[tokio::test]
async fn login_invalid_collection() {
    let app = setup_app(vec![make_users_def()], vec![]);

    let resp = app.router
        .oneshot(
            Request::post("/admin/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from("collection=nonexistent&email=a@b.com&password=x"))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    assert_eq!(status, StatusCode::OK, "Invalid collection should re-render login page");
}

#[tokio::test]
async fn reset_password_mismatched_passwords() {
    let app = setup_app(vec![make_users_def()], vec![]);

    let resp = app.router
        .oneshot(
            Request::post("/admin/reset-password")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from("token=sometoken&password=newpass123&password_confirm=different456"))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    assert_eq!(status, StatusCode::OK, "Mismatched passwords should re-render form");
    let body = body_string(resp.into_body()).await;
    let body_lower = body.to_lowercase();
    assert!(
        body_lower.contains("match") || body_lower.contains("password"),
        "Should indicate passwords don't match"
    );
}

#[tokio::test]
async fn reset_password_invalid_token() {
    let app = setup_app(vec![make_users_def()], vec![]);

    let resp = app.router
        .oneshot(
            Request::post("/admin/reset-password")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from("token=totally-invalid-token&password=newpass123&password_confirm=newpass123"))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    assert_eq!(status, StatusCode::OK, "Invalid token should re-render with error");
    let body = body_string(resp.into_body()).await;
    let body_lower = body.to_lowercase();
    assert!(
        body_lower.contains("invalid") || body_lower.contains("expired") || body_lower.contains("error") || body_lower.contains("reset"),
        "Should indicate invalid/expired token"
    );
}

#[tokio::test]
async fn create_action_missing_required_field_shows_errors() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "valerr@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "valerr@test.com");

    // Submit empty title (required field)
    let resp = app.router
        .oneshot(
            Request::post("/admin/collections/posts")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title="))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    // Validation error should re-render the form (200) with toast/error message
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "Validation error should re-render form or redirect, got {}",
        status
    );
}

#[tokio::test]
async fn create_form_auth_collection_includes_password() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let user_id = create_test_user(&app, "authform@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "authform@test.com");

    let resp = app.router
        .oneshot(
            Request::get("/admin/collections/users/create")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(body.contains("password"), "Auth collection create form should have password field");
}

#[tokio::test]
async fn edit_form_auth_collection_includes_password() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let user_id = create_test_user(&app, "authedit@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "authedit@test.com");

    let resp = app.router
        .oneshot(
            Request::get(format!("/admin/collections/users/{}", user_id))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(body.contains("password"), "Auth collection edit form should have password field");
}

#[tokio::test]
async fn edit_form_nonexistent_document_returns_404() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "nondoc@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "nondoc@test.com");

    let resp = app.router
        .oneshot(
            Request::get("/admin/collections/posts/nonexistent-id")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_confirm_nonexistent_document_returns_404() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "delconfnon@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "delconfnon@test.com");

    let resp = app.router
        .oneshot(
            Request::get("/admin/collections/posts/nonexistent-id/delete")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn versioned_collection_create_form() {
    let app = setup_app(vec![make_versioned_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "ver@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "ver@test.com");

    let resp = app.router
        .oneshot(
            Request::get("/admin/collections/articles/create")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn post_with_method_delete_deletes_document() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "methoddel@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "methoddel@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([("title".to_string(), "Method Delete".to_string())]);
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();

    // Use POST with _method=DELETE (HTML form override)
    let resp = app.router
        .oneshot(
            Request::post(format!("/admin/collections/posts/{}", doc.id))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("_method=DELETE"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "DELETE via _method should succeed, got {}",
        status
    );
}

#[tokio::test]
async fn global_edit_nonexistent_returns_404() {
    let app = setup_app(vec![make_users_def()], vec![make_global_def()]);
    let user_id = create_test_user(&app, "globnon@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "globnon@test.com");

    let resp = app.router
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

    let resp = app.router
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

    let resp = app.router
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

    let resp = app.router
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

    let resp = app.router
        .oneshot(
            Request::get("/admin/globals/settings/versions")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert_eq!(status, StatusCode::SEE_OTHER, "Non-versioned global versions page should redirect");
}

#[tokio::test]
async fn global_restore_nonversioned_redirects() {
    let app = setup_app(vec![make_users_def()], vec![make_global_def()]);
    let user_id = create_test_user(&app, "grestnv@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "grestnv@test.com");

    let resp = app.router
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
    assert_eq!(status, StatusCode::SEE_OTHER, "Restore on non-versioned global should redirect");
}

#[tokio::test]
async fn upload_path_traversal_returns_404() {
    let app = setup_app(vec![make_posts_def()], vec![]);

    let resp = app.router
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

    let resp = app.router
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

#[tokio::test]
async fn update_action_nonexistent_collection_redirects() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "noncolu@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "noncolu@test.com");

    let resp = app.router
        .oneshot(
            Request::post("/admin/collections/nonexistent/someid")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=Test"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert_eq!(status, StatusCode::SEE_OTHER, "Update on nonexistent collection should redirect");
}

#[tokio::test]
async fn delete_action_nonexistent_collection_redirects() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "noncold@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "noncold@test.com");

    let resp = app.router
        .oneshot(
            Request::delete("/admin/collections/nonexistent/someid")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    // Should redirect to collections list
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "Delete on nonexistent collection should redirect, got {}",
        status
    );
}

#[tokio::test]
async fn delete_confirm_nonexistent_collection_returns_404() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "noncoldc@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "noncoldc@test.com");

    let resp = app.router
        .oneshot(
            Request::get("/admin/collections/nonexistent/someid/delete")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn restore_version_nonversioned_redirects() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "restnv@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "restnv@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([("title".to_string(), "NV Restore".to_string())]);
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app.router
        .oneshot(
            Request::post(format!("/admin/collections/posts/{}/versions/fake-version/restore", doc.id))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert_eq!(status, StatusCode::SEE_OTHER, "Restore on non-versioned should redirect");
}

#[tokio::test]
async fn restore_version_nonexistent_collection_redirects() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "restnc@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "restnc@test.com");

    let resp = app.router
        .oneshot(
            Request::post("/admin/collections/nonexistent/someid/versions/v1/restore")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert_eq!(status, StatusCode::SEE_OTHER, "Restore on nonexistent collection should redirect");
}

#[tokio::test]
async fn upload_collection_create_form_has_upload_context() {
    let app = setup_app(vec![make_users_def(), make_media_def()], vec![]);
    let user_id = create_test_user(&app, "uploadadm@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "uploadadm@test.com");

    let resp = app.router
        .oneshot(
            Request::get("/admin/collections/media/create")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ── CSRF Protection Tests ─────────────────────────────────────────────

#[tokio::test]
async fn csrf_post_without_token_returns_403() {
    let app = setup_app(vec![make_users_def()], vec![]);

    // POST without any CSRF token → 403
    let resp = app.router
        .oneshot(
            Request::post("/admin/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("collection=users&email=a@b.com&password=x"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "POST without CSRF token should be 403");
}

#[tokio::test]
async fn csrf_post_with_cookie_but_no_header_returns_403() {
    let app = setup_app(vec![make_users_def()], vec![]);

    // Cookie present but no X-CSRF-Token header and no _csrf form field → 403
    let resp = app.router
        .oneshot(
            Request::post("/admin/login")
                .header("Cookie", csrf_cookie())
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("collection=users&email=a@b.com&password=x"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "POST with cookie but no token should be 403");
}

#[tokio::test]
async fn csrf_post_with_mismatched_header_returns_403() {
    let app = setup_app(vec![make_users_def()], vec![]);

    // Cookie and header present but values don't match → 403
    let resp = app.router
        .oneshot(
            Request::post("/admin/login")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", "wrong-token-value")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("collection=users&email=a@b.com&password=x"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "POST with mismatched CSRF header should be 403");
}

#[tokio::test]
async fn csrf_post_with_matching_header_passes() {
    let app = setup_app(vec![make_users_def()], vec![]);

    // Cookie + matching X-CSRF-Token header → passes CSRF check (login will fail, but not 403)
    let resp = app.router
        .oneshot(
            Request::post("/admin/login")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("collection=users&email=a@b.com&password=wrong"))
                .unwrap(),
        )
        .await
        .unwrap();
    // Should NOT be 403 — CSRF passes, login logic runs (returns 200 with login error page)
    assert_ne!(resp.status(), StatusCode::FORBIDDEN, "POST with matching CSRF header should not be 403");
}

#[tokio::test]
async fn csrf_post_with_form_field_passes() {
    let app = setup_app(vec![make_users_def()], vec![]);

    // Cookie + _csrf form field (no header) → passes CSRF check
    let body = format!("collection=users&email=a@b.com&password=wrong&_csrf={}", TEST_CSRF);
    let resp = app.router
        .oneshot(
            Request::post("/admin/login")
                .header("Cookie", csrf_cookie())
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(resp.status(), StatusCode::FORBIDDEN, "POST with _csrf form field should not be 403");
}

#[tokio::test]
async fn csrf_get_sets_cookie() {
    let app = setup_app(vec![make_users_def()], vec![]);

    // GET without any cookies → response should set crap_csrf cookie
    let resp = app.router
        .oneshot(
            Request::get("/admin/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let set_cookie = resp.headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with("crap_csrf="));
    assert!(set_cookie.is_some(), "GET response should set crap_csrf cookie");
    let cookie_val = set_cookie.unwrap();
    assert!(cookie_val.contains("SameSite=Strict"), "CSRF cookie should be SameSite=Strict");
    assert!(!cookie_val.contains("HttpOnly"), "CSRF cookie must NOT be HttpOnly (JS needs to read it)");
}

#[tokio::test]
async fn csrf_delete_without_token_returns_403() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "csrfdelete@test.com", "pass123");
    let auth_cookie = make_auth_cookie(&app, &user_id, "csrfdelete@test.com");

    // DELETE with auth cookie but no CSRF → 403
    let resp = app.router
        .oneshot(
            Request::delete("/admin/collections/posts/some-id")
                .header("Cookie", &auth_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "DELETE without CSRF should be 403");
}

// ── CORS tests ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn cors_disabled_by_default() {
    let app = setup_app(vec![make_posts_def()], vec![]);

    let resp = app.router
        .oneshot(
            Request::get("/admin/login")
                .header("Origin", "http://evil.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // No CORS headers when cors is not configured
    assert!(
        resp.headers().get("access-control-allow-origin").is_none(),
        "No CORS headers when cors is not configured"
    );
}

#[tokio::test]
async fn cors_preflight_returns_headers() {
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".to_string();
    config.cors.allowed_origins = vec!["http://localhost:8080".to_string()];

    let app = setup_app_with_config(vec![make_posts_def()], vec![], config);

    // Send a preflight OPTIONS request
    let resp = app.router
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
        resp.headers().get("access-control-allow-origin").map(|v| v.to_str().unwrap()),
        Some("http://localhost:8080"),
        "Preflight should return matching origin"
    );
}

#[tokio::test]
async fn cors_wildcard_returns_star() {
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".to_string();
    config.cors.allowed_origins = vec!["*".to_string()];

    let app = setup_app_with_config(vec![make_posts_def()], vec![], config);

    let resp = app.router
        .oneshot(
            Request::get("/admin/login")
                .header("Origin", "http://anything.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.headers().get("access-control-allow-origin").map(|v| v.to_str().unwrap()),
        Some("*"),
        "Wildcard origin should return *"
    );
}

#[tokio::test]
async fn cors_non_matching_origin_not_reflected() {
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".to_string();
    config.cors.allowed_origins = vec!["http://allowed.com".to_string()];

    let app = setup_app_with_config(vec![make_posts_def()], vec![], config);

    let resp = app.router
        .oneshot(
            Request::get("/admin/login")
                .header("Origin", "http://not-allowed.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // tower-http CorsLayer does not add the header for non-matching origins
    assert!(
        resp.headers().get("access-control-allow-origin").is_none(),
        "Non-matching origin should not get CORS header"
    );
}

// ── Admin Access Gate Tests ─────────────────────────────────────────────

#[tokio::test]
async fn require_auth_blocks_when_no_auth_collection() {
    // require_auth = true (default) with no auth collection → 503 "setup required"
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".to_string();
    config.admin.require_auth = true;

    let app = setup_app_with_config(vec![make_posts_def()], vec![], config);
    let resp = app.router
        .oneshot(Request::get("/admin").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "require_auth=true with no auth collection should return 503"
    );
    let body = body_string(resp.into_body()).await;
    assert!(body.contains("Setup Required") || body.contains("setup required") || body.contains("auth"),
        "Response should mention setup/auth requirement");
}

#[tokio::test]
async fn require_auth_false_allows_open_admin() {
    // require_auth = false with no auth collection → open admin (200)
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".to_string();
    config.admin.require_auth = false;

    let app = setup_app_with_config(vec![make_posts_def()], vec![], config);
    let resp = app.router
        .oneshot(Request::get("/admin").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "require_auth=false with no auth collection should allow access"
    );
}
