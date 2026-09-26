//! Forgot/reset-password integration tests for admin HTTP handlers.
//!
//! Covers: forgot-password page + action, reset-password tokens, rate
//! limiting, and translated password-policy errors (reset page + admin
//! user forms).

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::used_underscore_binding,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal
)]

mod admin_auth_support;

use std::net::SocketAddr;

use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

use crap_cms::{
    config::CrapConfig,
    core::rate_limit::IP_RESET_PASSWORD_KEYSPACE,
    db::query::{self, TokenGrant},
};

use admin_auth_support::{
    TEST_CSRF, TestApp, auth_and_csrf, body_string, create_test_user, csrf_cookie,
    make_auth_cookie, make_users_def, setup_app, setup_app_with_config,
};

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
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::from("collection=users&email=nonexistent@test.com"))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER || status == StatusCode::FOUND,
        "Forgot password should return 200 or redirect, never error, got {status}"
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
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
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
        "Expected 200 or redirect for invalid token, got {status}"
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
        query::set_reset_token(
            &conn,
            &TokenGrant::builder("users", &user_id, expired_token, past_exp).build(),
        )
        .unwrap();
    }

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/reset-password")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::from(format!(
                    "collection=users&token={expired_token}&password=newpass123&password_confirm=newpass123"
                )))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK, "the form re-renders");
    let body = body_string(resp.into_body()).await;
    assert!(
        body.contains("Reset link has expired"),
        "an expired link must read as expired: {}",
        &body[..body.len().min(400)]
    );

    // Regression: the admin reset committed whatever a refused attempt had
    // written, while gRPC rolled it back. A refused attempt writes nothing on
    // either surface: the (expired) token row is untouched.
    assert!(reset_token_stored(&app, expired_token));
}

/// Whether `token` is still stored on a `users` row, expired or not.
fn reset_token_stored(app: &TestApp, token: &str) -> bool {
    let conn = app.pool.get().unwrap();

    query::find_by_reset_token(&conn, &make_users_def(), token, None)
        .unwrap()
        .is_some()
}

#[tokio::test]
async fn reset_password_valid_flow() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let user_id = create_test_user(&app, "reset@test.com", "oldpass123");

    let valid_token = "valid-reset-token-67890";
    {
        let conn = app.pool.get().unwrap();
        let future_exp = chrono::Utc::now().timestamp() + 3600;
        query::set_reset_token(
            &conn,
            &TokenGrant::builder("users", &user_id, valid_token, future_exp).build(),
        )
        .unwrap();
    }

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!("/admin/reset-password?token={valid_token}"))
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
        .clone()
        .oneshot(
            Request::post("/admin/reset-password")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::from(format!(
                    "token={valid_token}&password=newpass456&password_confirm=newpass456"
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND,
        "Successful password reset should redirect, got {status}"
    );
    if let Some(location) = resp.headers().get("location") {
        let loc = location.to_str().unwrap_or("");
        assert!(
            loc.contains("login") && loc.contains("success"),
            "Should redirect to login with success, got {loc}"
        );
    }
    assert!(
        !reset_token_stored(&app, valid_token),
        "a used reset token must be consumed"
    );
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
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
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

/// Regression: a password-confirmation mismatch is a user typo, not a token
/// guess. The gate sits AFTER the local mismatch/policy checks, so a mismatch
/// must not consume rate-limit budget — even seeded to one-below-threshold, a
/// mismatch leaves the limiter un-blocked.
#[tokio::test]
async fn reset_password_mismatch_does_not_count_against_ip_limiter() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let ip = "127.0.0.1";

    for _ in 0..19 {
        let _ = app.ip_forgot_password_limiter.check_and_block(ip);
    }
    assert!(!app.ip_forgot_password_limiter.is_blocked(ip));

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/reset-password")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::from(
                    "token=sometoken&password=newpass123&password_confirm=different456",
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "mismatch re-renders the form (no redirect, no block)"
    );
    assert!(
        !app.ip_forgot_password_limiter.is_blocked(ip),
        "a password-mismatch typo must NOT advance the rate limiter"
    );
}

/// Regression: a genuine reset attempt (matching passwords, valid policy) with a
/// wrong token IS a token guess and counts against the handler's OWN per-IP
/// keyspace (`IP_RESET_PASSWORD_KEYSPACE`, derived from the forgot-password IP
/// limiter with `rescoped`) atomically — deliberately NOT the forgot-password
/// counter itself, so
/// reset-token guesses and the forgot-password request flow don't drain each
/// other's budget. Seeded to one-below-threshold, one such attempt must trip it.
#[tokio::test]
async fn reset_password_wrong_token_counts_against_ip_limiter() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let ip = "127.0.0.1";

    // Derive the handler's limiter exactly as it does: same backend and
    // thresholds as the forgot-password IP limiter, its own keyspace.
    let reset_limiter = app
        .ip_forgot_password_limiter
        .rescoped(IP_RESET_PASSWORD_KEYSPACE);

    for _ in 0..19 {
        let _ = reset_limiter.check_and_block(ip);
    }
    assert!(!reset_limiter.is_blocked(ip));

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/reset-password")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::from(
                    "token=wrong-token&password=newpass123&password_confirm=newpass123",
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let _ = resp; // outcome (invalid-token error vs block) is indistinguishable
    assert!(
        reset_limiter.is_blocked(ip),
        "a wrong-token reset attempt must advance the scoped IP limiter to its threshold"
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
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
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
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
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
        "Should show minimum password length error, got: {body}"
    );
}

/// An app whose admin UI renders in German: an anonymous page and a user
/// without saved settings both fall back to `locale.default_locale`.
fn setup_german_app() -> TestApp {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;
    config.locale.default_locale = "de".to_string();
    setup_app_with_config(vec![make_users_def()], vec![], config)
}

/// Regression: a password-policy violation on the reset page showed the
/// English message whatever the UI locale.
#[tokio::test]
async fn reset_password_policy_error_is_translated() {
    let app = setup_german_app();

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/reset-password")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::from(
                    "token=sometoken&password=ab&password_confirm=ab",
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_string(resp.into_body()).await;
    assert!(
        body.contains("Das Passwort muss mindestens 8 Zeichen lang sein"),
        "{body}"
    );
}

/// A signed-in admin's session cookie.
fn admin_cookie(app: &TestApp) -> String {
    let admin_id = create_test_user(app, "admin@test.com", "pass1234");

    make_auth_cookie(app, &admin_id, "admin@test.com")
}

/// POST an admin user form with `cookie`'s session; returns the body.
async fn post_user_form(app: &TestApp, cookie: &str, path: &str, form: &str) -> String {
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post(path)
                .header("cookie", auth_and_csrf(cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(form.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    body_string(resp.into_body()).await
}

/// Regression: the admin create form checked the password policy itself and
/// toasted the English message; the service's `password` field error is now
/// what the re-rendered form shows, in the viewer's UI locale.
#[tokio::test]
async fn admin_create_password_policy_error_is_translated() {
    let app = setup_german_app();
    let cookie = admin_cookie(&app);

    let short = post_user_form(
        &app,
        &cookie,
        "/admin/collections/users",
        "email=new@test.com&password=ab",
    )
    .await;
    assert!(
        short.contains("Das Passwort muss mindestens 8 Zeichen lang sein"),
        "{short}"
    );

    let empty = post_user_form(
        &app,
        &cookie,
        "/admin/collections/users",
        "email=other@test.com&password=",
    )
    .await;
    assert!(
        empty.contains("Das Passwort darf nicht leer sein"),
        "{empty}"
    );
}

/// Regression: the admin edit form toasted the English policy message; the
/// service's field error now renders in the viewer's UI locale.
#[tokio::test]
async fn admin_update_password_policy_error_is_translated() {
    let app = setup_german_app();
    let cookie = admin_cookie(&app);
    let target = create_test_user(&app, "target@test.com", "pass1234");

    let body = post_user_form(
        &app,
        &cookie,
        &format!("/admin/collections/users/{target}"),
        "email=target@test.com&password=ab",
    )
    .await;
    assert!(
        body.contains("Das Passwort muss mindestens 8 Zeichen lang sein"),
        "{body}"
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
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
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
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
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
