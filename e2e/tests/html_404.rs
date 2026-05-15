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

//! Not-found behavior: requests for unregistered collections, unknown
//! globals, and missing items should return 404 (or a sensible redirect)
//! — never 500 and never leak unrelated content.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crap_cms_e2e::helpers::*;

// ── unknown_collection_returns_404 ───────────────────────────────────────

#[tokio::test]
async fn unknown_collection_list_returns_404() {
    let HtmlTestCtx { app, cookie, .. } =
        setup_html_test(vec![make_users_def()], vec![], "nf1@test.com", "pass123");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/collections/nonexistent")
                .header("Cookie", auth_and_csrf(&cookie))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "unknown collection should 404"
    );
}

// ── unknown_item_id_returns_404 ──────────────────────────────────────────

#[tokio::test]
async fn unknown_item_id_returns_404() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_users_def(), make_posts_def()],
        vec![],
        "nf2@test.com",
        "pass123",
    );

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/collections/posts/this-id-does-not-exist")
                .header("Cookie", auth_and_csrf(&cookie))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "unknown item id should 404"
    );
}

// ── unknown_global_returns_404 ───────────────────────────────────────────

#[tokio::test]
async fn unknown_global_returns_404() {
    let HtmlTestCtx { app, cookie, .. } =
        setup_html_test(vec![make_users_def()], vec![], "nf3@test.com", "pass123");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/globals/nonexistent")
                .header("Cookie", auth_and_csrf(&cookie))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "unknown global should 404"
    );
}
