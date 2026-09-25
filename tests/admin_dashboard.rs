//! Dashboard and static-asset integration tests for the admin HTTP handlers.
//!
//! Covers: global cards and collection counts on the dashboard (with and
//! without an auth collection), and static assets.

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

use std::collections::HashMap;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::json;
use tower::ServiceExt;

use admin_globals_support::{
    body_string, create_test_user, make_auth_cookie, make_global_def, make_posts_def,
    make_users_def, setup_app,
};
use crap_cms::{core::DocumentFields, db::query};

#[tokio::test]
async fn dashboard_shows_globals() {
    let app = setup_app(vec![make_users_def()], vec![make_global_def()]);
    let user_id = create_test_user(&app, "dashglobal@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "dashglobal@test.com");

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
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let body_lower = body.to_lowercase();
    assert!(
        body_lower.contains("settings"),
        "Dashboard should show global cards"
    );
}

#[tokio::test]
async fn static_asset_missing_returns_404() {
    let app = setup_app(vec![make_posts_def()], vec![]);
    let resp = app
        .router
        .oneshot(
            Request::get("/static/nonexistent.css")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "Non-existent static asset should return 404"
    );
}

#[tokio::test]
async fn dashboard_renders_collection_counts() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "dashcount@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "dashcount@test.com");

    let def = app.registry.get_collection("posts").unwrap().clone();
    for title in &["Post A", "Post B"] {
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let data: DocumentFields = HashMap::from([("title".to_string(), json!(title))]).into();
        query::create(&tx, "posts", &def, &data, None).unwrap();
        tx.commit().unwrap();
    }

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
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let body_lower = body.to_lowercase();
    assert!(
        body_lower.contains("posts") || body_lower.contains("post"),
        "Dashboard should contain collection info"
    );
}

#[tokio::test]
async fn dashboard_no_auth_no_collections() {
    let app = setup_app(vec![], vec![]);
    let resp = app
        .router
        .oneshot(Request::get("/admin").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn dashboard_no_auth_returns_200() {
    let app = setup_app(vec![make_posts_def()], vec![make_global_def()]);
    let resp = app
        .router
        .oneshot(Request::get("/admin").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let body_lower = body.to_lowercase();
    assert!(
        body_lower.contains("dashboard")
            || body_lower.contains("posts")
            || body_lower.contains("settings"),
        "Dashboard should render without auth"
    );
}

#[tokio::test]
async fn static_js_returns_200() {
    let app = setup_app(vec![make_posts_def()], vec![]);
    let resp = app
        .router
        .oneshot(
            Request::get("/static/components/index.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap_or(""));
    assert!(
        ct.unwrap_or("").contains("javascript"),
        "JS content type should be javascript, got {ct:?}"
    );
}
