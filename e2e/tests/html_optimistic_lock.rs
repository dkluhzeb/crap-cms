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

//! Concurrent-edit semantics. Today the admin write path is
//! **last-write-wins** — there's no optimistic-locking check, so two
//! sessions editing the same document race silently and the last
//! `POST /admin/collections/{slug}/{id}` to land wins. This file pins
//! that behavior so when optimistic locking is added (currently
//! tracked as a P1 gap), the test forces an explicit update rather
//! than silently passing the wrong assertion.

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

// ── concurrent_updates_last_write_wins ───────────────────────────────────
//
// CURRENT BEHAVIOR: two sequential POST updates from "different sessions"
// (same auth cookie, but doing the round-trip the way a real concurrent
// edit would: A loads form → B loads form → A submits → B submits) end
// with B's state. No 409, no conflict marker.
//
// When optimistic locking lands, this test SHOULD fail — change it to
// expect a conflict response from B's submission instead.

#[tokio::test]
async fn concurrent_updates_last_write_wins() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_concurrent_posts_def(), make_users_def()],
        vec![],
        "concur@test.com",
        "pass1234",
    );
    let post_id = seed_post(&app, "Original Title");

    // Both A and B "load" the doc (we just need a baseline — the load
    // would be GET /admin/collections/posts/{id} which we don't need to
    // exercise here since the issue is at submit time).

    // Session A submits its update.
    let resp_a = app
        .router
        .clone()
        .oneshot(
            Request::post(format!("/admin/collections/posts/{post_id}"))
                .header("Cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=Edit+from+A"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp_a.status().is_success() || resp_a.status().is_redirection(),
        "session A update should succeed, got: {}",
        resp_a.status()
    );

    // Session B submits its update with NO knowledge of A's edit.
    let resp_b = app
        .router
        .clone()
        .oneshot(
            Request::post(format!("/admin/collections/posts/{post_id}"))
                .header("Cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=Edit+from+B"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp_b.status().is_success() || resp_b.status().is_redirection(),
        "session B update should also succeed under last-write-wins; got: {}",
        resp_b.status()
    );

    // Final state: B's edit landed.
    let body = get_edit_form(&app, "posts", &post_id, &cookie).await;
    assert!(
        body.contains(r#"value="Edit from B""#),
        "last-write-wins: B's edit should be the final state"
    );
    assert!(
        !body.contains(r#"value="Edit from A""#),
        "A's edit should be overwritten under last-write-wins"
    );
    assert!(
        !body.contains(r#"value="Original Title""#),
        "Original should be gone after two updates"
    );
}

fn make_concurrent_posts_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def.timestamps = true;
    def.admin = AdminConfig {
        use_as_title: Some("title".to_string()),
        ..AdminConfig::default()
    };
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
    ];
    def
}

fn seed_post(app: &TestApp, title: &str) -> String {
    let def = app.registry.get_collection("posts").unwrap().clone();
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields = HashMap::from([("title".to_string(), json!(title))]).into();
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();
    doc.id.to_string()
}

async fn get_edit_form(app: &TestApp, slug: &str, id: &str, cookie: &str) -> String {
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!("/admin/collections/{slug}/{id}"))
                .header("Cookie", auth_and_csrf(cookie))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    body_string(resp.into_body()).await
}
