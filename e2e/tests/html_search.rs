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

//! `GET /admin/api/search/{slug}` — typeahead JSON endpoint used by
//! relationship pickers and upload field selection. Returns an array
//! of `{id, title, …}` results matching the `q` query parameter.

use std::collections::HashMap;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use crap_cms::core::DocumentFields;
use crap_cms::core::collection::*;
use crap_cms::core::field::*;
use crap_cms::db::query;
use crap_cms::db::query::fts::fts_upsert;
use crap_cms_e2e::helpers::*;

// ── search_returns_matching_docs ─────────────────────────────────────────

#[tokio::test]
async fn search_returns_matching_docs() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_searchable_posts_def(), make_users_def()],
        vec![],
        "search1@test.com",
        "pass123",
    );
    seed_post(&app, "Alpha Centauri");
    seed_post(&app, "Beta Pictoris");
    seed_post(&app, "Alpha Pegasi");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/api/search/posts?q=alpha")
                .header("Cookie", auth_and_csrf(&cookie))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let parsed: Value = serde_json::from_str(&body).expect("response is JSON");
    let arr = parsed.as_array().expect("response is array");

    // Response items are {"id", "label", ...} — the title is exposed
    // as "label" (relationship-picker UI uses it as the display string).
    let labels: Vec<&str> = arr
        .iter()
        .filter_map(|v| v.get("label")?.as_str())
        .collect();
    assert!(labels.contains(&"Alpha Centauri"), "got labels: {labels:?}");
    assert!(labels.contains(&"Alpha Pegasi"), "got labels: {labels:?}");
    assert!(
        !labels.contains(&"Beta Pictoris"),
        "Beta should not match q=alpha; got labels: {labels:?}"
    );
}

// ── search_empty_query_returns_all_up_to_limit ───────────────────────────

#[tokio::test]
async fn search_empty_query_returns_all_up_to_limit() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_searchable_posts_def(), make_users_def()],
        vec![],
        "search2@test.com",
        "pass123",
    );
    for i in 0..5 {
        seed_post(&app, &format!("Item {i}"));
    }

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/api/search/posts?limit=3")
                .header("Cookie", auth_and_csrf(&cookie))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let parsed: Value = serde_json::from_str(&body).expect("response is JSON");
    let arr = parsed.as_array().expect("response is array");

    assert_eq!(
        arr.len(),
        3,
        "limit=3 should return 3 results, got {}",
        arr.len()
    );
}

// ── search_unknown_slug_returns_empty_array ──────────────────────────────

#[tokio::test]
async fn search_unknown_slug_returns_empty_array() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_users_def()],
        vec![],
        "search3@test.com",
        "pass123",
    );

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/api/search/nonexistent?q=foo")
                .header("Cookie", auth_and_csrf(&cookie))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert_eq!(body.trim(), "[]", "unknown slug should return empty array");
}

fn make_searchable_posts_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def.timestamps = true;
    def.admin = AdminConfig {
        use_as_title: Some("title".to_string()),
        list_searchable_fields: vec!["title".to_string()],
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
    // `query::create` doesn't fire the FTS upsert that the service layer
    // does — call it manually so the doc is discoverable by search.
    fts_upsert(&tx, "posts", &doc, Some(&def)).expect("fts upsert");
    tx.commit().unwrap();
    doc.id.to_string()
}
