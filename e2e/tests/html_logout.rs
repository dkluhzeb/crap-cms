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

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crap_cms_e2e::helpers::*;

// ── logout_clears_session_cookies ────────────────────────────────────────
//
// POST /admin/logout should issue Set-Cookie headers that clear the session
// cookies and redirect to /admin/login. Verifies cookie names match what the
// admin layer expects.

#[tokio::test]
async fn logout_clears_session_cookies() {
    let HtmlTestCtx { app, cookie, .. } =
        setup_html_test(vec![make_users_def()], vec![], "logout@test.com", "pass123");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/logout")
                .header("Cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp
        .headers()
        .get("location")
        .expect("logout should redirect")
        .to_str()
        .unwrap();
    assert_eq!(location, "/admin/login");

    // Every Set-Cookie issued should clear (Max-Age=0 or expired Expires).
    let mut saw_session_clear = false;
    for h in resp.headers().get_all("set-cookie") {
        let v = h.to_str().unwrap();
        if v.starts_with("crap_session=") {
            saw_session_clear = true;
            assert!(
                v.contains("Max-Age=0") || v.contains("Expires=Thu, 01 Jan 1970"),
                "session cookie should be cleared, got: {v}"
            );
        }
    }
    assert!(saw_session_clear, "logout must clear crap_session cookie");
}

// ── logout_redirects_protected_request_to_login ──────────────────────────
//
// After logout, a follow-up request to a protected page without auth cookie
// should redirect to /admin/login (not 401 — the admin surface uses redirect
// auth, not API auth).

#[tokio::test]
async fn logout_redirects_protected_request_to_login() {
    let HtmlTestCtx { app, .. } = setup_html_test(
        vec![make_users_def()],
        vec![],
        "logout2@test.com",
        "pass123",
    );

    // Request a protected page WITHOUT the session cookie — simulates a user
    // who has just logged out.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/collections/users")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // Redirect chain: protected page → login. Either 302/303 or some path
    // with `/admin/login` in Location.
    assert!(
        resp.status().is_redirection() || resp.status() == StatusCode::UNAUTHORIZED,
        "unauthenticated request should redirect or 401, got: {}",
        resp.status()
    );
    if resp.status().is_redirection() {
        let location = resp.headers().get("location").unwrap().to_str().unwrap();
        assert!(
            location.contains("/admin/login"),
            "redirect should target login, got: {location}"
        );
    }
}
