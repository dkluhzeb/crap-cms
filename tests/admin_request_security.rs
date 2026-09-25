//! Request-security integration tests for the admin HTTP router.
//!
//! Covers: CSRF protection, CORS headers, and the admin access gate when no
//! auth collection exists.

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

mod admin_globals_support;

use std::net::SocketAddr;

use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

use admin_globals_support::{
    TEST_CSRF, body_string, create_test_user, csrf_cookie, make_auth_cookie, make_posts_def,
    make_users_def, setup_app, setup_app_with_config,
};
use crap_cms::config::CrapConfig;

#[tokio::test]
async fn csrf_post_without_token_returns_403() {
    let app = setup_app(vec![make_users_def()], vec![]);

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::from("collection=users&email=a@b.com&password=x"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "POST without CSRF token should be 403"
    );
}

#[tokio::test]
async fn csrf_post_with_cookie_but_no_header_returns_403() {
    let app = setup_app(vec![make_users_def()], vec![]);

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/login")
                .header("Cookie", csrf_cookie())
                .header("content-type", "application/x-www-form-urlencoded")
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::from("collection=users&email=a@b.com&password=x"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "POST with cookie but no token should be 403"
    );
}

#[tokio::test]
async fn csrf_post_with_mismatched_header_returns_403() {
    let app = setup_app(vec![make_users_def()], vec![]);

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/login")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", "wrong-token-value")
                .header("content-type", "application/x-www-form-urlencoded")
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::from("collection=users&email=a@b.com&password=x"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "POST with mismatched CSRF header should be 403"
    );
}

#[tokio::test]
async fn csrf_post_with_matching_header_passes() {
    let app = setup_app(vec![make_users_def()], vec![]);

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/login")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::from("collection=users&email=a@b.com&password=wrong"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "POST with matching CSRF header should not be 403"
    );
}

#[tokio::test]
async fn csrf_post_with_form_field_passes() {
    let app = setup_app(vec![make_users_def()], vec![]);

    let body = format!("collection=users&email=a@b.com&password=wrong&_csrf={TEST_CSRF}");
    let resp = app
        .router
        .oneshot(
            Request::post("/admin/login")
                .header("Cookie", csrf_cookie())
                .header("content-type", "application/x-www-form-urlencoded")
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "POST with _csrf form field should not be 403"
    );
}

#[tokio::test]
async fn csrf_get_sets_cookie() {
    let app = setup_app(vec![make_users_def()], vec![]);

    let resp = app
        .router
        .oneshot(Request::get("/admin/login").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let set_cookie = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with("crap_csrf="));
    assert!(
        set_cookie.is_some(),
        "GET response should set crap_csrf cookie"
    );
    let cookie_val = set_cookie.unwrap();
    assert!(
        cookie_val.contains("SameSite=Strict"),
        "CSRF cookie should be SameSite=Strict"
    );
    assert!(
        !cookie_val.contains("HttpOnly"),
        "CSRF cookie must NOT be HttpOnly (JS needs to read it)"
    );
}

#[tokio::test]
async fn csrf_delete_without_token_returns_403() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "csrfdelete@test.com", "pass123");
    let auth_cookie = make_auth_cookie(&app, &user_id, "csrfdelete@test.com");

    let resp = app
        .router
        .oneshot(
            Request::delete("/admin/collections/posts/some-id")
                .header("Cookie", &auth_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "DELETE without CSRF should be 403"
    );
}

#[tokio::test]
async fn cors_disabled_by_default() {
    let app = setup_app(vec![make_posts_def()], vec![]);

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/login")
                .header("Origin", "http://evil.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        resp.headers().get("access-control-allow-origin").is_none(),
        "No CORS headers when cors is not configured"
    );
}

#[tokio::test]
async fn cors_preflight_returns_headers() {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.cors.allowed_origins = vec!["http://localhost:8080".to_string()];

    let app = setup_app_with_config(vec![make_posts_def()], vec![], config);

    let resp = app
        .router
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/admin/login")
                .header("Origin", "http://localhost:8080")
                .header("Access-Control-Request-Method", "POST")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.headers()
            .get("access-control-allow-origin")
            .map(|v| v.to_str().unwrap()),
        Some("http://localhost:8080"),
        "Preflight should return matching origin"
    );
}

#[tokio::test]
async fn cors_wildcard_returns_star() {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.cors.allowed_origins = vec!["*".to_string()];

    let app = setup_app_with_config(vec![make_posts_def()], vec![], config);

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/login")
                .header("Origin", "http://anything.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.headers()
            .get("access-control-allow-origin")
            .map(|v| v.to_str().unwrap()),
        Some("*"),
        "Wildcard origin should return *"
    );
}

#[tokio::test]
async fn cors_non_matching_origin_not_reflected() {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.cors.allowed_origins = vec!["http://allowed.com".to_string()];

    let app = setup_app_with_config(vec![make_posts_def()], vec![], config);

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/login")
                .header("Origin", "http://not-allowed.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        resp.headers().get("access-control-allow-origin").is_none(),
        "Non-matching origin should not get CORS header"
    );
}

#[tokio::test]
async fn require_auth_blocks_when_no_auth_collection() {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = true;

    let app = setup_app_with_config(vec![make_posts_def()], vec![], config);
    let resp = app
        .router
        .oneshot(Request::get("/admin").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "require_auth=true with no auth collection should return 503"
    );
    let body = body_string(resp.into_body()).await;
    assert!(
        body.contains("Setup Required") || body.contains("setup required") || body.contains("auth"),
        "Response should mention setup/auth requirement"
    );
}

#[tokio::test]
async fn require_auth_false_allows_open_admin() {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;

    let app = setup_app_with_config(vec![make_posts_def()], vec![], config);
    let resp = app
        .router
        .oneshot(Request::get("/admin").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "require_auth=false with no auth collection should allow access"
    );
}
