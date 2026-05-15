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

//! Collection list sorting via the `?sort=` URL parameter. Ascending
//! is the bare field name; descending is `-field`. The list page
//! should reflect that order in the rendered rows.

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

// ── sort_asc_orders_rows_by_title ────────────────────────────────────────

#[tokio::test]
async fn sort_asc_orders_rows_by_title() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_sortable_posts_def(), make_users_def()],
        vec![],
        "sort1@test.com",
        "pass123",
    );
    create_post(&app, "Charlie Post");
    create_post(&app, "Alpha Post");
    create_post(&app, "Bravo Post");

    let body = get_list(&app, &cookie, "/admin/collections/posts?sort=title").await;
    let alpha = body.find("Alpha Post").expect("Alpha visible");
    let bravo = body.find("Bravo Post").expect("Bravo visible");
    let charlie = body.find("Charlie Post").expect("Charlie visible");

    assert!(
        alpha < bravo && bravo < charlie,
        "ascending sort should render Alpha < Bravo < Charlie; got positions {alpha}, {bravo}, {charlie}"
    );
}

// ── sort_desc_orders_rows_in_reverse ─────────────────────────────────────

#[tokio::test]
async fn sort_desc_orders_rows_in_reverse() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_sortable_posts_def(), make_users_def()],
        vec![],
        "sort2@test.com",
        "pass123",
    );
    create_post(&app, "Charlie Post");
    create_post(&app, "Alpha Post");
    create_post(&app, "Bravo Post");

    let body = get_list(&app, &cookie, "/admin/collections/posts?sort=-title").await;
    let alpha = body.find("Alpha Post").expect("Alpha visible");
    let bravo = body.find("Bravo Post").expect("Bravo visible");
    let charlie = body.find("Charlie Post").expect("Charlie visible");

    assert!(
        charlie < bravo && bravo < alpha,
        "descending sort should render Charlie < Bravo < Alpha; got positions alpha={alpha}, bravo={bravo}, charlie={charlie}"
    );
}

fn make_sortable_posts_def() -> CollectionDefinition {
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

fn create_post(app: &TestApp, title: &str) -> String {
    let def = app.registry.get_collection("posts").unwrap().clone();
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields = HashMap::from([("title".to_string(), json!(title))]).into();
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();
    doc.id.to_string()
}

async fn get_list(app: &TestApp, cookie: &str, url: &str) -> String {
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
