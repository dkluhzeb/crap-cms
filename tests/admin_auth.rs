//! Auth-related integration tests for admin HTTP handlers.
//!
//! Covers: login/logout, auth middleware, email verification, forgot/reset password.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use crap_cms::admin::AdminState;
use crap_cms::admin::server::build_router;
use crap_cms::admin::templates;
use crap_cms::config::CrapConfig;
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

    let translations = Arc::new(crap_cms::admin::translations::Translations::load(
        tmp.path(),
    ));
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
        forgot_password_limiter: std::sync::Arc::new(
            crap_cms::core::rate_limit::LoginRateLimiter::new(3, 900),
        ),
        has_auth,
        translations,
        shutdown: tokio_util::sync::CancellationToken::new(),
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
        .build();
    let token = auth::create_token(&claims, app.jwt_secret.as_ref()).unwrap();
    format!("crap_session={}", token)
}

const TEST_CSRF: &str = "test-csrf-token-12345";

fn csrf_cookie() -> String {
    format!("crap_csrf={}", TEST_CSRF)
}

#[allow(dead_code)]
fn auth_and_csrf(auth_cookie: &str) -> String {
    format!("{}; crap_csrf={}", auth_cookie, TEST_CSRF)
}

async fn body_string(body: Body) -> String {
    let bytes = body.collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn make_verify_users_def() -> CollectionDefinition {
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
    def.auth = Some(Auth {
        enabled: true,
        verify_email: true,
        ..Default::default()
    });
    def
}

// ── Login / Logout Tests ──────────────────────────────────────────────────

#[tokio::test]
async fn login_page_returns_200() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let resp = app
        .router
        .oneshot(Request::get("/admin/login").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(
        body.to_lowercase().contains("login"),
        "Login page should contain 'login'"
    );
}

#[tokio::test]
async fn login_action_invalid_credentials() {
    let app = setup_app(vec![make_users_def()], vec![]);
    create_test_user(&app, "user@test.com", "secret123");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(
                    "collection=users&email=user@test.com&password=wrong",
                ))
                .unwrap(),
        )
        .await
        .unwrap();
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

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(
                    "collection=users&email=valid@test.com&password=correct123",
                ))
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
    let cookie = resp
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap_or(""));
    assert!(cookie.is_some(), "Should set a session cookie");
    assert!(
        cookie.unwrap().contains("crap_session"),
        "Cookie should be crap_session"
    );
}

#[tokio::test]
async fn logout_clears_cookie() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let resp = app
        .router
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
    let cookie = resp
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap_or(""));
    if let Some(c) = cookie {
        assert!(
            c.contains("Max-Age=0")
                || c.contains("max-age=0")
                || c.contains("expires=Thu, 01 Jan 1970"),
            "Cookie should be expired: {}",
            c
        );
    }
}

// ── Auth Middleware Tests ─────────────────────────────────────────────────

#[tokio::test]
async fn protected_route_redirects_without_auth() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let resp = app
        .router
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
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "Protected route with valid cookie should return 200"
    );
}

#[tokio::test]
async fn no_auth_collections_skips_middleware() {
    let app = setup_app(vec![make_posts_def()], vec![]);
    let resp = app
        .router
        .oneshot(Request::get("/admin").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "No auth collections = no middleware = 200"
    );
}

#[tokio::test]
async fn login_locked_account() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let user_id = create_test_user(&app, "locked@test.com", "secret123");

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
                .body(Body::from(
                    "collection=users&email=locked@test.com&password=secret123",
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER || status == StatusCode::FOUND,
        "Expected 200 or redirect, got {}",
        status
    );

    if status == StatusCode::SEE_OTHER || status == StatusCode::FOUND {
        let location = resp
            .headers()
            .get("location")
            .map(|v| v.to_str().unwrap_or(""));
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
async fn login_wrong_password_shows_error() {
    let app = setup_app(vec![make_users_def()], vec![]);
    create_test_user(&app, "wrongpw@test.com", "correct123");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(
                    "collection=users&email=wrongpw@test.com&password=wrongpassword",
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    assert_eq!(
        status,
        StatusCode::OK,
        "Wrong password should re-render login page"
    );
    let body = body_string(resp.into_body()).await;
    let body_lower = body.to_lowercase();
    assert!(
        body_lower.contains("invalid")
            || body_lower.contains("error")
            || body_lower.contains("login"),
        "Should show error message on wrong password"
    );
}

#[tokio::test]
async fn login_nonexistent_email() {
    let app = setup_app(vec![make_users_def()], vec![]);
    create_test_user(&app, "exists@test.com", "secret123");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(
                    "collection=users&email=nope@test.com&password=secret123",
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    assert_eq!(
        status,
        StatusCode::OK,
        "Nonexistent email should re-render login page"
    );
}

#[tokio::test]
async fn login_invalid_collection() {
    let app = setup_app(vec![make_users_def()], vec![]);

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(
                    "collection=nonexistent&email=a@b.com&password=x",
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    assert_eq!(
        status,
        StatusCode::OK,
        "Invalid collection should re-render login page"
    );
}

// ── Email Verification Tests ──────────────────────────────────────────────

#[tokio::test]
async fn verify_email_invalid_token() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let resp = app
        .router
        .oneshot(
            Request::get("/admin/verify-email?token=badtoken&collection=users")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER || status == StatusCode::FOUND,
        "Expected 200 or redirect, got {}",
        status
    );
}

#[tokio::test]
async fn login_unverified_email() {
    let app = setup_app(vec![make_verify_users_def()], vec![]);

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

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(
                    "collection=vusers&email=unverified@test.com&password=secret123",
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "Unverified login should fail gracefully, got {}",
        status
    );
    if status == StatusCode::OK {
        let body = body_string(resp.into_body()).await;
        assert!(
            body.to_lowercase().contains("verify") || body.to_lowercase().contains("error"),
            "Login page should show verification error"
        );
    }
}

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

// ── Forgot Password Tests ─────────────────────────────────────────────────

#[tokio::test]
async fn forgot_password_page_returns_200() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let resp = app
        .router
        .oneshot(
            Request::get("/admin/forgot-password")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn forgot_password_action() {
    let app = setup_app(vec![make_users_def()], vec![]);

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
    let status = resp.status();
    assert_eq!(status, StatusCode::OK, "Forgot password should return 200");
    let body = body_string(resp.into_body()).await;
    let body_lower = body.to_lowercase();
    assert!(
        body_lower.contains("success")
            || body_lower.contains("sent")
            || body_lower.contains("check"),
        "Should show success message"
    );
}

// ── Reset Password Tests ──────────────────────────────────────────────────

#[tokio::test]
async fn reset_password_page_invalid_token() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let resp = app
        .router
        .oneshot(
            Request::get("/admin/reset-password?token=badtoken&collection=users")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER || status == StatusCode::FOUND,
        "Expected 200 or redirect for invalid token, got {}",
        status
    );
}

#[tokio::test]
async fn reset_password_expired_token() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let user_id = create_test_user(&app, "expired@test.com", "oldpass123");

    let expired_token = "expired-test-token-12345";
    {
        let conn = app.pool.get().unwrap();
        let past_exp = chrono::Utc::now().timestamp() - 3600;
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
    assert!(
        status == StatusCode::OK
            || status == StatusCode::SEE_OTHER
            || status == StatusCode::FOUND
            || status == StatusCode::UNPROCESSABLE_ENTITY,
        "Expected 200, redirect, or 422 for expired token, got {}",
        status
    );

    if status != StatusCode::SEE_OTHER && status != StatusCode::FOUND {
        let body = body_string(resp.into_body()).await;
        let body_lower = body.to_lowercase();
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

#[tokio::test]
async fn reset_password_valid_flow() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let user_id = create_test_user(&app, "reset@test.com", "oldpass123");

    let valid_token = "valid-reset-token-67890";
    {
        let conn = app.pool.get().unwrap();
        let future_exp = chrono::Utc::now().timestamp() + 3600;
        query::set_reset_token(&conn, "users", &user_id, valid_token, future_exp).unwrap();
    }

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
    assert!(
        !body.to_lowercase().contains("expired") && !body.to_lowercase().contains("invalid"),
        "Valid token should show reset form, not error"
    );

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
                .body(Body::from(
                    "token=sometoken&password=newpass123&password_confirm=different456",
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert_eq!(
        status,
        StatusCode::OK,
        "Mismatched passwords should re-render form"
    );
    let body = body_string(resp.into_body()).await;
    assert!(
        body.to_lowercase().contains("match"),
        "Should show 'passwords do not match' error"
    );
}

#[tokio::test]
async fn reset_password_mismatched_passwords() {
    let app = setup_app(vec![make_users_def()], vec![]);

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/reset-password")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(
                    "token=sometoken&password=newpass123&password_confirm=different456",
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    assert_eq!(
        status,
        StatusCode::OK,
        "Mismatched passwords should re-render form"
    );
    let body = body_string(resp.into_body()).await;
    let body_lower = body.to_lowercase();
    assert!(
        body_lower.contains("match") || body_lower.contains("password"),
        "Should indicate passwords don't match"
    );
}

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
                .body(Body::from(
                    "token=sometoken&password=ab&password_confirm=ab",
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert_eq!(
        status,
        StatusCode::OK,
        "Too-short password should re-render form"
    );
    let body = body_string(resp.into_body()).await;
    assert!(
        body.to_lowercase().contains("at least") && body.to_lowercase().contains("characters"),
        "Should show minimum password length error, got: {}",
        body
    );
}

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
                .body(Body::from(
                    "token=totally-fake-token&password=newpass123&password_confirm=newpass123",
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert_eq!(
        status,
        StatusCode::OK,
        "Invalid token should re-render form with error"
    );
    let body = body_string(resp.into_body()).await;
    assert!(
        body.to_lowercase().contains("invalid") || body.to_lowercase().contains("expired"),
        "Should show invalid/expired token error"
    );
}

#[tokio::test]
async fn reset_password_invalid_token() {
    let app = setup_app(vec![make_users_def()], vec![]);

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/reset-password")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(
                    "token=totally-invalid-token&password=newpass123&password_confirm=newpass123",
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    assert_eq!(
        status,
        StatusCode::OK,
        "Invalid token should re-render with error"
    );
    let body = body_string(resp.into_body()).await;
    let body_lower = body.to_lowercase();
    assert!(
        body_lower.contains("invalid")
            || body_lower.contains("expired")
            || body_lower.contains("error")
            || body_lower.contains("reset"),
        "Should indicate invalid/expired token"
    );
}
