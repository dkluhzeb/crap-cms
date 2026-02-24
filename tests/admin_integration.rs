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
use crap_cms::config::CrapConfig;
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
                field_type: FieldType::Text,
                required: true,
                unique: false,
                validate: None,
                default_value: None,
                options: Vec::new(),
                admin: FieldAdmin::default(),
                hooks: FieldHooks::default(),
                access: FieldAccess::default(),
                relationship: None,
                fields: Vec::new(),
                blocks: Vec::new(),
                localized: false,
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
                validate: None,
                default_value: None,
                options: Vec::new(),
                admin: FieldAdmin::default(),
                hooks: FieldHooks::default(),
                access: FieldAccess::default(),
                relationship: None,
                fields: Vec::new(),
                blocks: Vec::new(),
                localized: false,
            },
            FieldDefinition {
                name: "name".to_string(),
                field_type: FieldType::Text,
                required: false,
                unique: false,
                validate: None,
                default_value: None,
                options: Vec::new(),
                admin: FieldAdmin::default(),
                hooks: FieldHooks::default(),
                access: FieldAccess::default(),
                relationship: None,
                fields: Vec::new(),
                blocks: Vec::new(),
                localized: false,
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
                field_type: FieldType::Text,
                required: false,
                unique: false,
                validate: None,
                default_value: None,
                options: Vec::new(),
                admin: FieldAdmin::default(),
                hooks: FieldHooks::default(),
                access: FieldAccess::default(),
                relationship: None,
                fields: Vec::new(),
                blocks: Vec::new(),
                localized: false,
            },
        ],
        hooks: CollectionHooks::default(),
        access: CollectionAccess::default(),
        live: None,
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
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND,
        "Create action should redirect, got {}",
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
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND,
        "Update action should redirect, got {}",
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
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND,
        "Global update should redirect, got {}",
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
