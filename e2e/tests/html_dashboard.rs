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

//! Admin dashboard (GET /admin) — top-level landing page. Renders
//! collection and global summary cards. Most users land here right
//! after login.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crap_cms_e2e::{helpers::*, html};

// ── dashboard_renders_collection_and_global_cards ────────────────────────

#[tokio::test]
async fn dashboard_renders_collection_and_global_cards() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_users_def(), make_posts_def()],
        vec![make_settings_def()],
        "dash@test.com",
        "pass123",
    );

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin")
                .header("Cookie", auth_and_csrf(&cookie))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let doc = html::parse(&body);

    // Should show the user's registered collections somewhere on the page.
    assert!(
        body.contains("Posts") || body.contains("posts"),
        "dashboard should mention the Posts collection"
    );
    // And the global.
    assert!(
        body.contains("Settings") || body.contains("settings"),
        "dashboard should mention the Settings global"
    );
    // Links to the collection should exist.
    html::assert_exists(
        &doc,
        "a[href='/admin/collections/posts']",
        "dashboard should link to the posts collection",
    );
}

// ── dashboard_unauthenticated_redirects_to_login ─────────────────────────

#[tokio::test]
async fn dashboard_unauthenticated_redirects_to_login() {
    let HtmlTestCtx { app, .. } =
        setup_html_test(vec![make_users_def()], vec![], "dash2@test.com", "pass123");

    let resp = app
        .router
        .clone()
        .oneshot(Request::get("/admin").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert!(
        resp.status().is_redirection() || resp.status() == StatusCode::UNAUTHORIZED,
        "unauthenticated /admin should redirect or 401, got: {}",
        resp.status()
    );
}
