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

//! CSRF protection: mutating requests without the `crap_csrf` cookie or
//! a matching `X-CSRF-Token` header are rejected with 403. Browser
//! sessions rely on this gate — without it, a malicious site could
//! trigger writes via cross-origin form submissions.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crap_cms_e2e::helpers::*;

// ── post_without_csrf_cookie_rejected ────────────────────────────────────

#[tokio::test]
async fn post_without_csrf_cookie_rejected() {
    let HtmlTestCtx { app, cookie, .. } =
        setup_html_test(vec![make_users_def()], vec![], "csrf1@test.com", "pass123");

    // Strip the csrf cookie — keep only the session cookie.
    let session_only: String = cookie
        .split(';')
        .map(str::trim)
        .filter(|c| !c.starts_with("crap_csrf="))
        .collect::<Vec<_>>()
        .join("; ");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/logout")
                .header("Cookie", session_only)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "POST without CSRF cookie should be rejected"
    );
}

// ── post_with_mismatched_csrf_token_rejected ─────────────────────────────

#[tokio::test]
async fn post_with_mismatched_csrf_token_rejected() {
    let HtmlTestCtx { app, cookie, .. } =
        setup_html_test(vec![make_users_def()], vec![], "csrf2@test.com", "pass123");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/logout")
                .header("Cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", "not-the-cookie-value")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "POST with mismatched CSRF token should be rejected"
    );
}

// ── post_with_matching_csrf_token_accepted ───────────────────────────────
//
// Positive control: confirms the 403s above are gated on CSRF, not on
// some other broken precondition.

#[tokio::test]
async fn post_with_matching_csrf_token_accepted() {
    let HtmlTestCtx { app, cookie, .. } =
        setup_html_test(vec![make_users_def()], vec![], "csrf3@test.com", "pass123");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/logout")
                .header("Cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp.status().is_redirection(),
        "logout with matching CSRF should redirect, got: {}",
        resp.status()
    );
}
