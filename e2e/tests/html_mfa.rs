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

use crap_cms::core::collection::{Auth, MfaMode};
use crap_cms_e2e::helpers::*;
use crap_cms_e2e::{extract_mfa_code, wait_for_queued_email};

// ── mfa_email_full_flow ──────────────────────────────────────────────────
//
// Auth collection with `mfa = Email`. Correct password → redirect to
// /admin/mfa + crap_mfa_pending cookie. MFA-code email queued; the test
// reads the code, posts it to /admin/mfa, and verifies a session is
// created (redirect to dashboard).

#[tokio::test]
async fn mfa_email_full_flow() {
    let app = setup_app(vec![make_users_def_mfa_email()], vec![]);
    create_test_user(&app, "mfa@test.com", "pass1234");

    let resp = app
        .router
        .clone()
        .oneshot(login_request("mfa@test.com", "pass1234"))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::SEE_OTHER,
        "MFA-enabled login should redirect (to /admin/mfa)"
    );

    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert!(
        location.contains("/admin/mfa"),
        "redirect target should be /admin/mfa, got: {location}"
    );

    // Capture the mfa_pending cookie — needed for the POST /admin/mfa.
    let mfa_cookie = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|h| h.to_str().ok())
        .find(|v| v.starts_with("crap_mfa_pending="))
        .expect("login response should set crap_mfa_pending cookie")
        .to_string();
    let mfa_cookie_value = mfa_cookie.split(';').next().unwrap().trim().to_string();

    // Read the MFA code from the queued email.
    let email = wait_for_queued_email(&app, "mfa@test.com", Duration::from_secs(2))
        .expect("MFA email should be queued");
    assert_eq!(email.subject, "Your verification code");
    let code = extract_mfa_code(&email).expect("email should contain 6-digit code");
    assert_eq!(code.len(), 6);

    // Submit the code.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/mfa")
                .header("content-type", "application/x-www-form-urlencoded")
                .header(
                    "Cookie",
                    format!("crap_csrf={TEST_CSRF}; {mfa_cookie_value}"),
                )
                .header("X-CSRF-Token", TEST_CSRF)
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::from(format!("code={code}")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp.status().is_redirection(),
        "valid MFA code should redirect to dashboard, got: {}",
        resp.status()
    );

    // Session cookie should be set on the response.
    let saw_session = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|h| h.to_str().ok())
        .any(|v| v.starts_with("crap_session="));
    assert!(saw_session, "MFA success must set crap_session cookie");
}

// ── mfa_wrong_code_rejected ──────────────────────────────────────────────

#[tokio::test]
async fn mfa_wrong_code_rejected() {
    let app = setup_app(vec![make_users_def_mfa_email()], vec![]);
    create_test_user(&app, "mfawrong@test.com", "pass1234");

    let resp = app
        .router
        .clone()
        .oneshot(login_request("mfawrong@test.com", "pass1234"))
        .await
        .unwrap();
    let mfa_cookie = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|h| h.to_str().ok())
        .find(|v| v.starts_with("crap_mfa_pending="))
        .unwrap()
        .to_string();
    let mfa_cookie_value = mfa_cookie.split(';').next().unwrap().trim().to_string();

    // POST a wrong code.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/mfa")
                .header("content-type", "application/x-www-form-urlencoded")
                .header(
                    "Cookie",
                    format!("crap_csrf={TEST_CSRF}; {mfa_cookie_value}"),
                )
                .header("X-CSRF-Token", TEST_CSRF)
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::from("code=000000"))
                .unwrap(),
        )
        .await
        .unwrap();

    // Wrong code → re-render MFA page (200) or stay on it; should NOT set session.
    let session_set = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|h| h.to_str().ok())
        .any(|v| v.starts_with("crap_session=") && !v.contains("Max-Age=0"));
    assert!(!session_set, "wrong MFA code must not create a session");
}

fn make_users_def_mfa_email() -> crap_cms::core::collection::CollectionDefinition {
    let mut def = make_users_def();
    def.auth = Some(Auth {
        enabled: true,
        mfa: MfaMode::Email,
        ..Default::default()
    });
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
