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

//! Session-expiry enforcement at the server. The auth middleware compares
//! the JWT's `session_version` claim against the user's current
//! `_session_version` column. When they diverge — typically because the
//! user changed password or an admin forced a session invalidation — the
//! existing JWT must be rejected on the next request.
//!
//! (The client-side `<crap-session-guard>` component is timer-based and
//! warns *before* expiry; that surface is browser-only and lives in
//! `browser_session_expiry.rs` if/when we add it.)

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crap_cms::db::{DbConnection, DbValue};
use crap_cms_e2e::helpers::*;

// ── stale_jwt_blocked_after_session_version_bump ─────────────────────────

#[tokio::test]
async fn stale_jwt_blocked_after_session_version_bump() {
    let HtmlTestCtx {
        app,
        cookie,
        user_id,
    } = setup_html_test(vec![make_users_def()], vec![], "stale@test.com", "pass123");

    // Sanity: with current cookie, a protected page renders.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/collections/users")
                .header("Cookie", auth_and_csrf(&cookie))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "baseline: fresh session should access protected page"
    );

    // Bump _session_version on the user → existing JWT now has a stale
    // session_version claim. This is what `update_password` does in
    // production to invalidate all sessions after a password change.
    {
        let conn = app.pool.get().unwrap();
        let n = conn
            .execute(
                "UPDATE users SET _session_version = _session_version + 1 WHERE id = ?1",
                &[DbValue::Text(user_id.clone())],
            )
            .expect("bump _session_version");
        assert_eq!(n, 1, "should have updated one user row");
    }

    // Same cookie, same protected URL — should now be rejected.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/collections/users")
                .header("Cookie", auth_and_csrf(&cookie))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp.status() == StatusCode::UNAUTHORIZED
            || resp.status() == StatusCode::FORBIDDEN
            || resp.status().is_redirection(),
        "stale JWT must be rejected (401 / 403 / redirect to login), got: {}",
        resp.status()
    );
    if resp.status().is_redirection() {
        let location = resp.headers().get("location").unwrap().to_str().unwrap();
        assert!(
            location.contains("/admin/login"),
            "redirect target should be login, got: {location}"
        );
    }
}
