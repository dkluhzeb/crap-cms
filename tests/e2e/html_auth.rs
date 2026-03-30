use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crate::helpers::*;
use crate::html;

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
                .header("Cookie", format!("crap_csrf={}", TEST_CSRF))
                .header("X-CSRF-Token", TEST_CSRF)
                .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
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
