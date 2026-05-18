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

//! Admin-side e2e for the email-verify *consume* path.
//!
//! The full round-trip (`user create` → email queued → click link →
//! verified) splits across two surfaces: the *send* half belongs to the
//! CLI workstream because email-send is wired to `service::create_document`
//! → `maybe_send_verification`, which the CLI's `user create` triggers
//! directly. This file covers the admin half: a valid verification token
//! presented at GET /admin/verify-email consumes it and marks the user
//! verified. We plant the token directly via `query::set_verification_token`
//! to isolate from the email-rendering surface.

use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use tower::ServiceExt;

use crap_cms::core::collection::{Auth, CollectionDefinition};
use crap_cms::db::query;
use crap_cms_e2e::helpers::*;

// ── verify_email_valid_token_marks_verified ──────────────────────────────

#[tokio::test]
async fn verify_email_valid_token_marks_verified() {
    let app = setup_app(vec![make_users_def_verify_email()], vec![]);
    let user_id = create_test_user(&app, "verify@test.com", "pass1234");

    // Plant a verification token (what `service::email::send_verification_email`
    // would do, minus the email rendering / queueing).
    let token = "test-verify-token-abc123";
    let exp = Utc::now().timestamp() + 3600;
    {
        let conn = app.pool.get().unwrap();
        query::set_verification_token(&conn, "users", &user_id, token, exp)
            .expect("set verification token");
    }

    // Before verification: login is blocked because user is unverified.
    let resp = app
        .router
        .clone()
        .oneshot(login_request("verify@test.com", "pass1234"))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "unverified user login should NOT create a session (re-renders login page)"
    );

    // Click the verification link.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!("/admin/verify-email?token={token}"))
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert!(
        location.contains("/admin/login") && location.contains("success"),
        "verify-email should redirect to login with success flash, got: {location}"
    );

    // After verification: login works.
    let resp = app
        .router
        .clone()
        .oneshot(login_request("verify@test.com", "pass1234"))
        .await
        .unwrap();
    assert!(
        resp.status().is_redirection(),
        "verified user login should create a session (redirect), got: {}",
        resp.status()
    );
}

// ── verify_email_invalid_token_redirects_to_login ────────────────────────

#[tokio::test]
async fn verify_email_invalid_token_redirects_to_login() {
    let app = setup_app(vec![make_users_def_verify_email()], vec![]);
    create_test_user(&app, "badverify@test.com", "pass1234");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/verify-email?token=does-not-exist")
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // Invalid token → redirect back to login (without success flash).
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert!(location.contains("/admin/login"));
    assert!(
        !location.contains("success"),
        "invalid token should NOT show success flash, got: {location}"
    );
}

fn make_users_def_verify_email() -> CollectionDefinition {
    let mut def = make_users_def();
    def.auth = Some(Auth::enabled().map_password_login(|b| b.verify_email(true)));
    def
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
