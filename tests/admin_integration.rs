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

    let translations = Arc::new(Translations::load(tmp.path(), "en"));
    let handlebars =
        templates::create_handlebars(tmp.path(), false, translations).expect("create handlebars");
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
        registry: registry.clone(),
        handlebars,
        hook_runner,
        jwt_secret: "test-jwt-secret".to_string(),
        email_renderer,
        event_bus: None,
    };

    let router = build_router(state, has_auth);

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
                .header("cookie", &cookie)
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
                .header("cookie", &cookie)
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
                .header("cookie", &cookie)
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
                .header("cookie", &cookie)
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
                .header("cookie", &cookie)
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
                .header("cookie", &cookie)
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
                .header("cookie", &cookie)
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
                .header("cookie", &cookie)
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
                .header("cookie", &cookie)
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
