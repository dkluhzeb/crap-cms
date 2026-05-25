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

use std::collections::HashMap;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;
use tower::ServiceExt;

use crap_cms::core::DocumentFields;
use crap_cms::core::collection::*;
use crap_cms::core::field::*;
use crap_cms::db::query;
use crap_cms_e2e::helpers::*;

// ── soft_delete_moves_doc_to_trash ───────────────────────────────────────

#[tokio::test]
async fn soft_delete_moves_doc_to_trash() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_soft_posts_def(), make_users_def()],
        vec![],
        "trash@test.com",
        "pass1234",
    );
    let alpha = create_post(&app, "Alpha Post");
    let beta = create_post(&app, "Beta Post");

    // Soft-delete Alpha.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::delete(format!("/admin/collections/posts/{alpha}"))
                .header("Cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp.status().is_success() || resp.status().is_redirection(),
        "soft-delete should succeed, got: {}",
        resp.status()
    );

    // List page: Beta visible, Alpha NOT visible.
    let body = list_body(&app, &cookie, "/admin/collections/posts").await;
    assert!(body.contains("Beta Post"), "Beta should be in active list");
    assert!(
        !body.contains("Alpha Post"),
        "Alpha should be hidden from active list after soft-delete"
    );

    // Trash list: Alpha visible, Beta NOT visible.
    let body = list_body(&app, &cookie, "/admin/collections/posts?trash=1").await;
    assert!(body.contains("Alpha Post"), "Alpha should appear in trash");
    assert!(!body.contains("Beta Post"), "Beta should not be in trash");
    let _ = beta; // suppress unused warning
}

// ── undelete_restores_doc_to_active_list ─────────────────────────────────

#[tokio::test]
async fn undelete_restores_doc_to_active_list() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_soft_posts_def(), make_users_def()],
        vec![],
        "undelete@test.com",
        "pass1234",
    );
    let gamma = create_post(&app, "Gamma Post");

    // Soft-delete.
    let _ = app
        .router
        .clone()
        .oneshot(
            Request::delete(format!("/admin/collections/posts/{gamma}"))
                .header("Cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // Undelete.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post(format!("/admin/collections/posts/{gamma}/undelete"))
                .header("Cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp.status().is_success() || resp.status().is_redirection(),
        "undelete should succeed, got: {}",
        resp.status()
    );

    // Active list: Gamma visible again.
    let body = list_body(&app, &cookie, "/admin/collections/posts").await;
    assert!(
        body.contains("Gamma Post"),
        "Gamma should be back in active list after undelete"
    );
}

// ── empty_trash_purges_all_soft_deleted ──────────────────────────────────

#[tokio::test]
async fn empty_trash_purges_all_soft_deleted() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_soft_posts_def(), make_users_def()],
        vec![],
        "emptytrash@test.com",
        "pass1234",
    );
    let d1 = create_post(&app, "Delta One");
    let d2 = create_post(&app, "Delta Two");

    for id in [&d1, &d2] {
        let _ = app
            .router
            .clone()
            .oneshot(
                Request::delete(format!("/admin/collections/posts/{id}"))
                    .header("Cookie", auth_and_csrf(&cookie))
                    .header("X-CSRF-Token", TEST_CSRF)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
    }

    // Verify both in trash first.
    let body = list_body(&app, &cookie, "/admin/collections/posts?trash=1").await;
    assert!(body.contains("Delta One") && body.contains("Delta Two"));

    // Empty the trash.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/collections/posts/empty-trash")
                .header("Cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp.status().is_success() || resp.status().is_redirection(),
        "empty-trash should succeed, got: {}",
        resp.status()
    );

    // Trash list now empty.
    let body = list_body(&app, &cookie, "/admin/collections/posts?trash=1").await;
    assert!(
        !body.contains("Delta One") && !body.contains("Delta Two"),
        "trash should be empty after empty-trash"
    );
}

fn make_soft_posts_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def.timestamps = true;
    def.soft_delete = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
    ];
    def.admin = AdminConfig {
        use_as_title: Some("title".to_string()),
        ..AdminConfig::default()
    };
    def
}

fn create_post(app: &TestApp, title: &str) -> String {
    let def = app.registry.get_collection("posts").unwrap().clone();
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields = HashMap::from([("title".to_string(), json!(title))]).into();
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();
    doc.id.to_string()
}

async fn list_body(app: &TestApp, cookie: &str, url: &str) -> String {
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(url)
                .header("Cookie", auth_and_csrf(cookie))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "list page should render");
    body_string(resp.into_body()).await
}
