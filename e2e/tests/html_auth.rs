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

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

use crap_cms_e2e::{helpers::*, html};

// ── 21. login_page_renders_form ───────────────────────────────────────────

#[tokio::test]
async fn login_page_renders_form() {
    let app = setup_app(vec![make_users_def()], vec![]);

    let resp = app
        .router
        .oneshot(Request::get("/admin/login").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let doc = html::parse(&body);

    // Email input
    html::assert_exists(&doc, "input[name=\"email\"]", "email input");
    // Password input
    html::assert_exists(&doc, "input[name=\"password\"]", "password input");
    // Submit button
    html::assert_exists(&doc, "button[type=\"submit\"]", "submit button");
}

// ── 22. login_failure_shows_error ─────────────────────────────────────────

#[tokio::test]
async fn login_failure_shows_error() {
    let app = setup_app(vec![make_users_def()], vec![]);
    create_test_user(&app, "wrong@test.com", "correct123");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", format!("crap_csrf={TEST_CSRF}"))
                .header("X-CSRF-Token", TEST_CSRF)
                .extension(axum::extract::ConnectInfo(SocketAddr::from((
                    [127, 0, 0, 1],
                    0,
                ))))
                .body(Body::from(
                    "collection=users&email=wrong@test.com&password=wrongpassword",
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let doc = html::parse(&body);

    // Should show an error message
    html::assert_exists(
        &doc,
        ".auth-card__error",
        "login failure should show error message",
    );
}
