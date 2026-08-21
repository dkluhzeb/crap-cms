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

//! Ref-count delete protection: a doc with `_ref_count > 0` cannot be
//! hard-deleted, and the back-references endpoint surfaces the referring
//! documents so the user knows what's blocking the delete.

use std::collections::HashMap;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;
use tower::ServiceExt;

use crap_cms::core::DocumentFields;
use crap_cms::core::collection::*;
use crap_cms::core::field::*;
use crap_cms::db::{DbConnection, DbValue, query};
use crap_cms_e2e::helpers::*;

// ── hard_delete_blocked_when_referenced ──────────────────────────────────

#[tokio::test]
async fn hard_delete_blocked_when_referenced() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![
            make_categories_def(),
            make_rel_posts_def(),
            make_users_def(),
        ],
        vec![],
        "refdelete@test.com",
        "pass1234",
    );

    let cat_id = create_category(&app, "Tech");
    let _post_id = create_post(&app, "Article 1", &cat_id);
    // `query::create` doesn't fire `ref_count::after_create` — that's a
    // service-layer concern. Mirror its effect manually so the test
    // exercises the delete-block check.
    bump_ref_count(&app, "categories", &cat_id);

    // Try to hard-delete the category via the dialog path so we get a
    // structured JSON error back (the non-dialog path silently redirects
    // and the doc stays in place — a real UX bug but separate from this
    // test's scope).
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::delete(format!("/admin/collections/categories/{cat_id}"))
                .header("Cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("X-Delete-Dialog", "1")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("_action=hard_delete"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "ref-count violation should return 409 Conflict on the dialog path"
    );
    let body = body_string(resp.into_body()).await;
    assert!(
        body.contains("Cannot delete") && body.contains("referenced by"),
        "error JSON should explain the ref-count block, got: {body}"
    );

    // Category still exists in the list.
    let list_body = list_body(&app, &cookie, "/admin/collections/categories").await;
    assert!(
        list_body.contains("Tech"),
        "category should still exist in list after failed delete"
    );
}

// ── back_references_shows_referring_documents ────────────────────────────

#[tokio::test]
async fn back_references_shows_referring_documents() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![
            make_categories_def(),
            make_rel_posts_def(),
            make_users_def(),
        ],
        vec![],
        "backref@test.com",
        "pass1234",
    );

    let cat_id = create_category(&app, "Science");
    let _ = create_post(&app, "Quantum Paper", &cat_id);
    let _ = create_post(&app, "Black Hole Paper", &cat_id);

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!(
                "/admin/collections/categories/{cat_id}/back-references"
            ))
            .header("Cookie", auth_and_csrf(&cookie))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    // Response is metadata about what *kind* of references exist
    // (owner_slug + field_name + count + document_ids), not the full
    // documents — the UI fetches titles on demand.
    assert!(
        body.contains(r#""owner_slug":"posts""#),
        "expected posts as referring collection, got: {body}"
    );
    assert!(
        body.contains(r#""field_name":"category""#),
        "expected the category field to be reported, got: {body}"
    );
    assert!(
        body.contains(r#""count":2"#),
        "expected count=2 referring documents, got: {body}"
    );
}

fn make_categories_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("categories");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Category".to_string())),
        plural: Some(LocalizedString::Plain("Categories".to_string())),
    };
    def.timestamps = true;
    def.admin = AdminConfig {
        use_as_title: Some("name".to_string()),
        ..Default::default()
    };
    def.fields = vec![
        FieldDefinition::builder("name", FieldType::Text)
            .required(true)
            .build(),
    ];
    def
}

fn make_rel_posts_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def.timestamps = true;
    def.admin = AdminConfig {
        use_as_title: Some("title".to_string()),
        ..Default::default()
    };
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("category", FieldType::Relationship)
            .relationship(RelationshipConfig::new("categories", false))
            .build(),
    ];
    def
}

fn create_category(app: &TestApp, name: &str) -> String {
    let def = app.registry.get_collection("categories").unwrap().clone();
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields = HashMap::from([("name".to_string(), json!(name))]).into();
    let doc = query::create(&tx, "categories", &def, &data, None).unwrap();
    tx.commit().unwrap();
    doc.id.to_string()
}

fn create_post(app: &TestApp, title: &str, category_id: &str) -> String {
    let def = app.registry.get_collection("posts").unwrap().clone();
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields = HashMap::from([
        ("title".to_string(), json!(title)),
        ("category".to_string(), json!(category_id)),
    ])
    .into();
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();
    doc.id.to_string()
}

fn bump_ref_count(app: &TestApp, slug: &str, id: &str) {
    let conn = app.pool.get().unwrap();
    conn.execute(
        &format!("UPDATE \"{slug}\" SET _ref_count = _ref_count + 1 WHERE id = ?1"),
        &[DbValue::Text(id.to_string())],
    )
    .unwrap();
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
    assert_eq!(resp.status(), StatusCode::OK);
    body_string(resp.into_body()).await
}
