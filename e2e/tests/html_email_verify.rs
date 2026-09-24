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
//!
//! The self-service resend at /admin/resend-verification is covered here
//! too, end to end: it queues a real email whose link verifies the account.

use std::net::SocketAddr;
use std::time::Duration;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use tower::ServiceExt;

use crap_cms::config::CrapConfig;
use crap_cms::core::collection::{Auth, CollectionDefinition};
use crap_cms::db::query::{self, TokenGrant};
use crap_cms_e2e::helpers::*;
use crap_cms_e2e::{extract_token, find_queued_email, wait_for_queued_email};

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
        query::set_verification_token(
            &conn,
            &TokenGrant::builder("users", &user_id, token, exp).build(),
        )
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

// ── resend_verification ──────────────────────────────────────────────────

/// An app whose email transport is configured, so the resend actually
/// queues mail instead of logging a warning and returning.
fn setup_app_with_email(collections: Vec<CollectionDefinition>) -> TestApp {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;
    config.admin.dev_mode = true;
    config.email.smtp_host = "localhost".to_string();

    setup_app_with_config(collections, vec![], config)
}

/// Blank out the per-response CSP nonce so two renders of the same page
/// compare equal.
fn without_nonces(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;

    while let Some(start) = rest.find("nonce=\"") {
        let after = start + "nonce=\"".len();
        let Some(end) = rest[after..].find('"') else {
            break;
        };

        out.push_str(&rest[..after]);
        rest = &rest[after + end..];
    }

    out.push_str(rest);
    out
}

fn resend_request(email: &str) -> Request<Body> {
    Request::post("/admin/resend-verification")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("Cookie", format!("crap_csrf={TEST_CSRF}"))
        .header("X-CSRF-Token", TEST_CSRF)
        .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
        .body(Body::from(format!("collection=users&email={email}")))
        .unwrap()
}

/// The full self-service round trip: ask for a new link, receive it, click
/// it, and the account is verified.
#[tokio::test]
async fn resend_verification_mails_a_working_link() {
    let app = setup_app_with_email(vec![make_users_def_verify_email()]);
    create_test_user(&app, "resend@test.com", "pass1234");

    let resp = app
        .router
        .clone()
        .oneshot(resend_request("resend@test.com"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let email = wait_for_queued_email(&app, "resend@test.com", Duration::from_secs(2))
        .expect("a verification email should be queued");
    assert_eq!(email.subject, "Verify your email");

    let token =
        extract_token(&email, "/admin/verify-email").expect("the link should carry a token");

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
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert!(
        location.contains("success"),
        "the resent link should verify the account, got: {location}"
    );

    let resp = app
        .router
        .clone()
        .oneshot(login_request("resend@test.com", "pass1234"))
        .await
        .unwrap();
    assert!(
        resp.status().is_redirection(),
        "the account is verified now"
    );
}

/// An address nobody owns gets the same page as a real one, and no mail.
#[tokio::test]
async fn resend_verification_is_silent_about_unknown_addresses() {
    let app = setup_app_with_email(vec![make_users_def_verify_email()]);
    create_test_user(&app, "known@test.com", "pass1234");

    let known = app
        .router
        .clone()
        .oneshot(resend_request("known@test.com"))
        .await
        .unwrap();
    let unknown = app
        .router
        .clone()
        .oneshot(resend_request("stranger@test.com"))
        .await
        .unwrap();

    assert_eq!(known.status(), unknown.status());
    assert_eq!(
        without_nonces(&body_string(known.into_body()).await),
        without_nonces(&body_string(unknown.into_body()).await),
        "a registered and an unregistered address must render identically"
    );

    // Give the spawned task the same window the positive test uses, then
    // confirm nothing was queued for the address nobody owns.
    let _ = wait_for_queued_email(&app, "known@test.com", Duration::from_secs(2));
    assert!(
        find_queued_email(&app, "stranger@test.com").is_none(),
        "no mail may go to an unregistered address"
    );
}

/// With no collection requiring verification the route is a dead end, and
/// the login page does not advertise it.
#[tokio::test]
async fn resend_verification_is_hidden_when_nothing_needs_verifying() {
    let app = setup_app_with_email(vec![make_users_def()]);

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/resend-verification")
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(resp.status().is_redirection());

    let login = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/login")
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        !body_string(login.into_body())
            .await
            .contains("/admin/resend-verification"),
        "the login page must not link a page that can do nothing"
    );
}

/// With a verifying collection the login page offers the link.
#[tokio::test]
async fn login_page_offers_the_resend_link() {
    let app = setup_app_with_email(vec![make_users_def_verify_email()]);

    let login = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/login")
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        body_string(login.into_body())
            .await
            .contains("/admin/resend-verification")
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
