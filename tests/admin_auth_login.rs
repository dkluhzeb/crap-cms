//! Login-related integration tests for admin HTTP handlers.
//!
//! Covers: login page + CSRF cookie/CSP, login action (credentials, cookie
//! flags, locked accounts, unknown collections), and the `admin.access` gate
//! at login time.

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
    core::{
        HookRef,
        collection::{CollectionDefinition, GlobalDefinition},
    },
    db::query,
};

use admin_auth_support::{
    TEST_CSRF, TestApp, body_string, create_test_user, create_test_user_with_role, csrf_cookie,
    make_auth_cookie, make_users_def, setup_app, setup_app_in_dir, setup_app_with_config,
};

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

/// Regression test for audit finding M-3 — the `crap_csrf` cookie's
/// `Max-Age` must reflect `admin.csrf_cookie_lifetime` (default 86400),
/// not a hardcoded literal. Future refactors that drop the config read
/// would still set the same value by coincidence, but TOML-parse tests
/// in `config::server::tests` cover the non-default path.
#[tokio::test]
async fn csrf_cookie_max_age_matches_configured_default() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let resp = app
        .router
        .oneshot(Request::get("/admin/login").body(Body::empty()).unwrap())
        .await
        .unwrap();

    let cookie = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|s| s.starts_with("crap_csrf="))
        .expect("admin/login response must set crap_csrf cookie");

    assert!(
        cookie.contains("Max-Age=86400"),
        "crap_csrf cookie must carry configured Max-Age, got: {cookie}",
    );
    assert!(
        cookie.contains("SameSite=Strict"),
        "crap_csrf cookie must be SameSite=Strict, got: {cookie}",
    );
}

/// The `crap_csrf` cookie the response hands back, if any.
fn issued_csrf_cookie(resp: &axum::response::Response) -> Option<String> {
    resp.headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|s| s.starts_with("crap_csrf="))
        .map(std::string::ToString::to_string)
}

/// Regression: an `Authorization: Bearer` header with nothing after it is not
/// a bearer request — the evaluator ignores it and authenticates from the
/// session cookie instead. The CSRF middleware used to accept it as one and
/// wave the submit through unchecked.
#[tokio::test]
async fn an_empty_bearer_header_does_not_skip_the_csrf_check() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let user_id = create_test_user(&app, "bearer@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "bearer@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/users")
                .header("cookie", cookie)
                .header("authorization", "Bearer ")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("email=new@test.com&password=secret456"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "a cookie-authenticated submit still owes a CSRF token"
    );
}

/// Regression: once the `crap_csrf` cookie expired, every submit answered 403
/// without handing back a replacement — so the failure repeated until the user
/// reloaded a page by hand. The 403 now carries a fresh cookie.
#[tokio::test]
async fn a_missing_csrf_cookie_is_re_issued_on_the_refusal() {
    let app = setup_app(vec![make_users_def()], vec![]);

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::from("collection=users&email=a@b.c&password=x"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let cookie = issued_csrf_cookie(&resp).expect("the refusal must hand back a fresh token");
    assert!(
        cookie.contains("Max-Age=86400") && cookie.contains("SameSite=Strict"),
        "the re-issued cookie keeps the configured shape, got: {cookie}"
    );
}

/// Regression: a native form submit too large for the CSRF fallback to read
/// answered 403 "CSRF validation failed", sending the user after a token
/// problem that never existed. It is an oversized body — 413.
#[tokio::test]
async fn an_oversized_form_submit_is_too_large_not_a_csrf_failure() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let body = "x".repeat(3 * 1024 * 1024);

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/users")
                .header("cookie", csrf_cookie())
                .header("content-type", "application/x-www-form-urlencoded")
                .header("content-length", body.len().to_string())
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

/// Regression test for CSP hardening — the rendered page must carry a
/// per-request nonce in both the `Content-Security-Policy` header and
/// every inline `<script>`, and must not fall back to `'unsafe-inline'`
/// for scripts. Protects against future refactors that silently relax CSP.
#[tokio::test]
async fn login_page_csp_nonce_matches_inline_scripts() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let resp = app
        .router
        .oneshot(Request::get("/admin/login").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let csp = resp
        .headers()
        .get("content-security-policy")
        .expect("CSP header present")
        .to_str()
        .unwrap()
        .to_string();

    // Both `script-src` and `style-src` are free of `'unsafe-inline'`:
    //   - scripts use a per-request nonce
    //   - styles use constructable stylesheets in components, classes/`hidden`
    //     in templates, and HTMX's indicator-style injection is disabled via
    //     `htmx.config.includeIndicatorStyles=false` (the indicator CSS rules
    //     ship in `static/styles/base/reset.css` instead).
    let script_src = csp
        .split(';')
        .map(str::trim)
        .find(|d| d.starts_with("script-src "))
        .expect("script-src directive present");

    assert!(
        !script_src.contains("'unsafe-inline'"),
        "script-src must not include 'unsafe-inline' — got {script_src:?}",
    );

    let style_src = csp
        .split(';')
        .map(str::trim)
        .find(|d| d.starts_with("style-src "))
        .expect("style-src directive present");

    assert!(
        !style_src.contains("'unsafe-inline'"),
        "style-src must not include 'unsafe-inline' — got {style_src:?}",
    );

    let nonce_prefix = "'nonce-";
    let start = script_src
        .find(nonce_prefix)
        .expect("script-src carries a nonce directive");
    let after = &script_src[start + nonce_prefix.len()..];
    let end = after.find('\'').expect("nonce directive is closed");
    let nonce = &after[..end];
    assert!(!nonce.is_empty(), "nonce must not be empty");

    let body = body_string(resp.into_body()).await;
    let expected_attr = format!("nonce=\"{nonce}\"");
    assert!(
        body.contains(&expected_attr),
        "rendered HTML must carry matching nonce attribute {expected_attr:?}",
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
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
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
        "Expected 200 or redirect, got {status}"
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
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
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
        "Expected redirect, got {status}"
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
async fn login_sets_session_cookie_with_samesite_lax_by_default() {
    let app = setup_app(vec![make_users_def()], vec![]);
    create_test_user(&app, "samesite-lax@test.com", "correct123");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::from(
                    "collection=users&email=samesite-lax@test.com&password=correct123",
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let session_cookie = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with("crap_session="))
        .expect("login should set crap_session cookie");

    assert!(
        session_cookie.contains("SameSite=Lax"),
        "default session cookie must be SameSite=Lax, got: {session_cookie}"
    );
    assert!(
        !session_cookie.contains("SameSite=Strict"),
        "default must not be Strict, got: {session_cookie}"
    );
}

#[tokio::test]
async fn login_sets_session_cookie_with_samesite_strict_when_configured() {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;
    config.auth.session_cookie_samesite = crap_cms::config::SessionCookieSameSite::Strict;

    let app = setup_app_with_config(vec![make_users_def()], vec![], config);
    create_test_user(&app, "samesite-strict@test.com", "correct123");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::from(
                    "collection=users&email=samesite-strict@test.com&password=correct123",
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let session_cookie = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with("crap_session="))
        .expect("login should set crap_session cookie");

    assert!(
        session_cookie.contains("SameSite=Strict"),
        "configured session cookie must be SameSite=Strict, got: {session_cookie}"
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
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
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
        "Expected 200 or redirect, got {status}"
    );

    if status == StatusCode::SEE_OTHER || status == StatusCode::FOUND {
        let location = resp
            .headers()
            .get("location")
            .map(|v| v.to_str().unwrap_or(""));
        if let Some(loc) = location {
            assert!(
                loc.contains("login"),
                "Locked account should redirect to login, not {loc}"
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
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
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
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
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
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
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

// ── admin.access gate ────────────────────────────────────────────────

fn setup_app_with_admin_gate(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
) -> TestApp {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;
    config.admin.access = Some(HookRef::new("access.admin_only"));

    let tmp = tempfile::tempdir().expect("tempdir");

    // Write the admin_only.lua access function into the config dir
    let access_dir = tmp.path().join("access");
    std::fs::create_dir_all(&access_dir).unwrap();
    std::fs::write(
        access_dir.join("admin_only.lua"),
        r#"return function(context)
    return context.user ~= nil and context.user.role == "admin"
end"#,
    )
    .unwrap();

    setup_app_in_dir(collections, globals, config, tmp)
}

/// Regression: admin.access gate must block non-admin users at login time,
/// not just on subsequent page loads after the session cookie is set.
#[tokio::test]
async fn login_denied_by_admin_access_gate() {
    let app = setup_app_with_admin_gate(vec![make_users_def()], vec![]);
    create_test_user_with_role(&app, "nonadmin@test.com", "pass123", "user");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::from(
                    "collection=users&email=nonadmin@test.com&password=pass123",
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "Non-admin user should be denied at login by admin.access gate"
    );

    // Should NOT have a session cookie set
    let has_session = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .any(|v| v.to_str().unwrap_or("").contains("crap_session"));
    assert!(
        !has_session,
        "Session cookie must not be set for denied users"
    );
}

/// Admin users should still be able to log in when admin.access is configured.
#[tokio::test]
async fn login_allowed_by_admin_access_gate() {
    let app = setup_app_with_admin_gate(vec![make_users_def()], vec![]);
    create_test_user_with_role(&app, "admin@test.com", "pass123", "admin");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::from(
                    "collection=users&email=admin@test.com&password=pass123",
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND,
        "Admin user should be redirected after login, got {status}"
    );

    let has_session = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .any(|v| v.to_str().unwrap_or("").contains("crap_session"));
    assert!(has_session, "Admin user should get a session cookie");
}
