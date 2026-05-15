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

use std::net::SocketAddr;
use std::time::Duration;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crap_cms_e2e::helpers::*;
use crap_cms_e2e::{extract_token, wait_for_queued_email};

// ── password_reset_full_flow ─────────────────────────────────────────────
//
// POST /admin/forgot-password queues a reset email; the test reads the
// queue, extracts the token, posts a new password to /admin/reset-password,
// and verifies the user can log in with the new password and not the old.

#[tokio::test]
async fn password_reset_full_flow() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let _user_id = create_test_user(&app, "reset@test.com", "oldpass123");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/forgot-password")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", format!("crap_csrf={TEST_CSRF}"))
                .header("X-CSRF-Token", TEST_CSRF)
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::from("collection=users&email=reset@test.com"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Wait for the spawn_blocking task to insert the queued email.
    let email = wait_for_queued_email(&app, "reset@test.com", Duration::from_secs(2))
        .expect("password reset email should be queued");
    assert_eq!(email.subject, "Reset your password");

    let token =
        extract_token(&email, "/admin/reset-password").expect("reset link should contain a token");

    // Submit the new password.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/reset-password")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", format!("crap_csrf={TEST_CSRF}"))
                .header("X-CSRF-Token", TEST_CSRF)
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::from(format!(
                    "token={token}&password=newpass1234&password_confirm=newpass1234"
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp.status().is_redirection(),
        "successful reset should redirect, got: {}",
        resp.status()
    );

    // Old password no longer works.
    let resp = app
        .router
        .clone()
        .oneshot(login_request("reset@test.com", "oldpass123"))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "old password login should not redirect"
    );
    // (200 OK = login page re-rendered with error, not a session redirect)

    // New password works.
    let resp = app
        .router
        .clone()
        .oneshot(login_request("reset@test.com", "newpass1234"))
        .await
        .unwrap();
    assert!(
        resp.status().is_redirection(),
        "new password login should redirect to dashboard, got: {}",
        resp.status()
    );
}

// ── password_reset_rejects_mismatched_confirmation ───────────────────────

#[tokio::test]
async fn password_reset_rejects_mismatched_confirmation() {
    let app = setup_app(vec![make_users_def()], vec![]);
    create_test_user(&app, "mismatch@test.com", "oldpass123");

    let _ = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/forgot-password")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", format!("crap_csrf={TEST_CSRF}"))
                .header("X-CSRF-Token", TEST_CSRF)
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::from("collection=users&email=mismatch@test.com"))
                .unwrap(),
        )
        .await
        .unwrap();

    let email = wait_for_queued_email(&app, "mismatch@test.com", Duration::from_secs(2))
        .expect("password reset email should be queued");
    let token =
        extract_token(&email, "/admin/reset-password").expect("reset link should contain a token");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/reset-password")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", format!("crap_csrf={TEST_CSRF}"))
                .header("X-CSRF-Token", TEST_CSRF)
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::from(format!(
                    "token={token}&password=newpass1234&password_confirm=differentpw9"
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    // Mismatched confirmation → re-renders form with error (200), no redirect.
    assert_eq!(resp.status(), StatusCode::OK);
}

fn login_request(email: &str, password: &str) -> Request<Body> {
    Request::post("/admin/login")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("Cookie", format!("crap_csrf={TEST_CSRF}"))
        .header("X-CSRF-Token", TEST_CSRF)
        .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
        .body(Body::from(format!(
            "collection=users&email={email}&password={password}"
        )))
        .unwrap()
}
