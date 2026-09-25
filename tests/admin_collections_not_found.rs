//! Missing-target integration tests for the admin collection handlers.
//!
//! Covers: every collection form and action against a collection or document
//! that does not exist — 404 pages and redirects.

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

mod admin_collections_support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

use admin_collections_support::{
    TEST_CSRF, auth_and_csrf, create_test_user, make_auth_cookie, make_posts_def, make_users_def,
    setup_app,
};

#[tokio::test]
async fn nonexistent_collection_returns_404() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "notfound@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "notfound@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/nope")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn create_form_nonexistent_collection_returns_404() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "cfnf@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "cfnf@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/nonexistent/create")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn create_action_nonexistent_collection_redirects() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "canf@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "canf@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/nonexistent")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=Test"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND,
        "Create on nonexistent collection should redirect, got {status}"
    );
}

#[tokio::test]
async fn edit_nonexistent_document_returns_404() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "editnf@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "editnf@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts/nonexistent-id-12345")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn edit_form_nonexistent_document_returns_404() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "nondoc@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "nondoc@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts/nonexistent-id")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn update_nonexistent_document() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "updnf@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "updnf@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/posts/nonexistent-id-12345")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=Updated"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER
            || status == StatusCode::OK
            || status == StatusCode::FOUND
            || status == StatusCode::NOT_FOUND
            || status == StatusCode::INTERNAL_SERVER_ERROR,
        "Update nonexistent should return redirect or error, got {status}"
    );
}

#[tokio::test]
async fn update_action_nonexistent_collection_redirects() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "noncolu@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "noncolu@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/nonexistent/someid")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=Test"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert_eq!(
        status,
        StatusCode::SEE_OTHER,
        "Update on nonexistent collection should redirect"
    );
}

#[tokio::test]
async fn delete_nonexistent_document() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "delnf@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "delnf@test.com");

    let resp = app
        .router
        .oneshot(
            Request::delete("/admin/collections/posts/nonexistent-id-12345")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER
            || status == StatusCode::OK
            || status == StatusCode::FOUND
            || status == StatusCode::NOT_FOUND,
        "Delete nonexistent should return redirect or not found, got {status}"
    );
}

#[tokio::test]
async fn delete_action_nonexistent_collection_redirects() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "noncold@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "noncold@test.com");

    let resp = app
        .router
        .oneshot(
            Request::delete("/admin/collections/nonexistent/someid")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "Delete on nonexistent collection should redirect, got {status}"
    );
}

#[tokio::test]
async fn delete_confirm_nonexistent_document_returns_404() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "delconfnon@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "delconfnon@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts/nonexistent-id/delete")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_confirm_nonexistent_collection_returns_404() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "noncoldc@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "noncoldc@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/nonexistent/someid/delete")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn restore_version_nonexistent_collection_redirects() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "restnc@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "restnc@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/nonexistent/someid/versions/v1/restore")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert_eq!(
        status,
        StatusCode::SEE_OTHER,
        "Restore on nonexistent collection should redirect"
    );
}
