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

//! htmx partial navigation — admin nav links target `#main`, so an
//! htmx-issued page GET (`HX-Request: true`, `HX-Target: main`) must get
//! only `<title>` + main content, while direct loads and htmx
//! history-restore requests get the full document.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crap_cms_e2e::helpers::*;

async fn get_admin(ctx: &HtmlTestCtx, headers: &[(&str, &str)]) -> (StatusCode, String) {
    let mut req = Request::get("/admin").header("Cookie", auth_and_csrf(&ctx.cookie));
    for (k, v) in headers {
        req = req.header(*k, *v);
    }

    let resp = ctx
        .app
        .router
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();

    let status = resp.status();
    let body = body_string(resp.into_body()).await;
    (status, body)
}

fn setup() -> HtmlTestCtx {
    setup_html_test(
        vec![make_users_def(), make_posts_def()],
        vec![],
        "partialnav@test.com",
        "pass123",
    )
}

// ── htmx nav targeting #main gets the partial ────────────────────────────

#[tokio::test]
async fn htmx_main_nav_returns_partial_with_title() {
    let ctx = setup();

    let (status, body) = get_admin(&ctx, &[("HX-Request", "true"), ("HX-Target", "main")]).await;
    assert_eq!(status, StatusCode::OK);

    // Partial: a <title> (htmx applies it to document.title) + content...
    assert!(body.contains("<title>"), "partial must carry a <title>");
    // ...but none of the document shell.
    assert!(!body.contains("<!DOCTYPE html>"), "no doctype in a partial");
    assert!(!body.contains("<body>"), "no body element in a partial");
    assert!(
        !body.contains("id=\"crap-i18n\""),
        "no head data-islands in a partial"
    );
    assert!(
        !body.contains("class=\"sidebar"),
        "the sidebar stays out of partial responses"
    );
}

// ── direct browser load gets the full document ───────────────────────────

#[tokio::test]
async fn direct_load_returns_full_document() {
    let ctx = setup();

    let (status, body) = get_admin(&ctx, &[]).await;
    assert_eq!(status, StatusCode::OK);

    assert!(body.contains("<!DOCTYPE html>"));
    assert!(body.contains("<body>"));
    assert!(body.contains("id=\"crap-i18n\""));
}

// ── history-restore requests get the full document ───────────────────────

/// htmx history-cache misses re-fetch the page expecting a COMPLETE
/// document to extract the history element from — a partial here would
/// blank the page on back/forward after cache eviction.
#[tokio::test]
async fn history_restore_returns_full_document() {
    let ctx = setup();

    let (status, body) = get_admin(
        &ctx,
        &[
            ("HX-Request", "true"),
            ("HX-Target", "main"),
            ("HX-History-Restore-Request", "true"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    assert!(body.contains("<!DOCTYPE html>"));
    assert!(body.contains("<body>"));
}

// ── htmx requests NOT targeting #main stay full ──────────────────────────

/// Non-navigation htmx requests (form posts into dialogs, fragment loads
/// with their own targets) must not accidentally receive the nav partial.
#[tokio::test]
async fn htmx_other_target_returns_full_document() {
    let ctx = setup();

    let (status, body) = get_admin(&ctx, &[("HX-Request", "true")]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<!DOCTYPE html>"));
}
