//! Session-related integration tests for admin HTTP handlers.
//!
//! Covers: session refresh, logout (cookie clearing + server-side revocation),
//! and the auth middleware.

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

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

use crap_cms::{
    config::CrapConfig,
    core::{
        HookRef,
        collection::{Activation, AuthMethod, SurfaceSet},
    },
    db::query,
};

use admin_auth_support::{
    TEST_CSRF, TestApp, auth_and_csrf, create_test_user, csrf_cookie, make_auth_cookie,
    make_posts_def, make_users_def, setup_app, setup_app_in_dir,
};

// ── Session Refresh Tests ─────────────────────────────────────────────────

/// POST `/admin/api/session-refresh` with the session `cookie`, if any (plus
/// a matching CSRF token), and extra `headers`.
async fn post_session_refresh(
    app: &TestApp,
    cookie: Option<&str>,
    headers: &[(&str, &str)],
) -> axum::response::Response {
    let cookies = cookie.map_or_else(csrf_cookie, auth_and_csrf);
    let mut request = Request::post("/admin/api/session-refresh")
        .header("Cookie", cookies)
        .header("X-CSRF-Token", TEST_CSRF);

    for (name, value) in headers {
        request = request.header(*name, *value);
    }

    app.router
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

fn sets_session_cookie(resp: &axum::response::Response) -> bool {
    resp.headers()
        .get_all("set-cookie")
        .iter()
        .any(|v| v.to_str().unwrap_or("").starts_with("crap_session="))
}

/// A request the session cookie authenticated gets its session extended.
#[tokio::test]
async fn session_refresh_extends_a_cookie_session() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let user_id = create_test_user(&app, "refresh@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "refresh@test.com");

    let resp = post_session_refresh(&app, Some(&cookie), &[]).await;

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert!(
        sets_session_cookie(&resp),
        "a fresh session cookie is issued"
    );
}

/// Regression: a request a custom strategy authenticated carried internal
/// claims the refresh endpoint minted a signed session JWT from — exchanging
/// the strategy credential for a long-lived token usable on other surfaces,
/// surviving the strategy credential's revocation.
#[tokio::test]
async fn session_refresh_refuses_a_strategy_authenticated_request() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let hook_dir = tmp.path().join("auth");
    std::fs::create_dir_all(&hook_dir).unwrap();
    std::fs::write(
        hook_dir.join("sso.lua"),
        r#"
return function(ctx)
    local uid = ctx.headers["x-sso-user"]
    if not uid then return nil end
    return { id = uid }
end
"#,
    )
    .unwrap();

    let mut users = make_users_def();
    users
        .auth
        .as_mut()
        .unwrap()
        .methods
        .push(AuthMethod::Strategy {
            name: "sso".into(),
            authenticate: HookRef::new("auth.sso"),
            activates_on: Activation::header("x-sso-user"),
            surfaces: SurfaceSet::admin_only(),
        });

    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;

    let app = setup_app_in_dir(vec![users], vec![], config, tmp);
    let user_id = create_test_user(&app, "sso@test.com", "pass123");

    // The strategy authenticates the request…
    let page = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin")
                .header("x-sso-user", &user_id)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::OK, "the strategy authenticates");

    // …but it has no cookie session to extend: no token is minted.
    let resp = post_session_refresh(&app, None, &[("x-sso-user", user_id.as_str())]).await;

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(
        !sets_session_cookie(&resp),
        "a strategy credential must not be exchanged for a session cookie"
    );
}

// ── Logout Tests ──────────────────────────────────────────────────────────

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
        "Expected redirect, got {status}"
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
            "Cookie should be expired: {c}"
        );
    }
}

fn session_version(app: &TestApp, user_id: &str) -> u64 {
    let conn = app.pool.get().unwrap();
    query::get_session_version(&conn, "users", user_id).unwrap()
}

/// Regression: logout only cleared the browser's cookies. The route sits
/// outside the auth layer, so no principal was ever resolved for it and the
/// server-side bump never ran — a captured JWT stayed valid until `exp`.
#[tokio::test]
async fn logout_revokes_the_session_it_was_called_with() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "logout@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "logout@test.com");
    let version_before = session_version(&app, &user_id);

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/logout")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);

    assert_eq!(
        session_version(&app, &user_id),
        version_before + 1,
        "logout must bump _session_version so the issued JWT is stale"
    );

    // The JWT issued before logout is now a definite failure on a protected
    // route: refused, and the dead cookie is cleared so the browser stops
    // sending it.
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
        StatusCode::SEE_OTHER,
        "a JWT issued before logout must be refused"
    );
    let cleared = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|c| c.starts_with("crap_session=") && c.contains("Max-Age=0"));
    assert!(
        cleared,
        "a stale session is a definite failure and clears the dead cookie"
    );
}

/// A session that no longer authenticates has nothing to retire, but the
/// browser must still be able to clear its cookies — the reason the route
/// stays outside the auth layer.
#[tokio::test]
async fn logout_with_a_dead_session_still_clears_cookies() {
    let app = setup_app(vec![make_users_def()], vec![]);

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/logout")
                .header("cookie", auth_and_csrf("crap_session=not-a-jwt"))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);

    let cleared = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|c| c.starts_with("crap_session=") && c.contains("Max-Age=0"));
    assert!(cleared, "logout must clear the session cookie regardless");
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
