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

//! Verify the version-restore round-trip: create doc, update it (which
//! snapshots the pre-update state as a version), restore the snapshot,
//! confirm the doc reverted.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crap_cms::core::collection::*;
use crap_cms::core::field::*;
use crap_cms::db::{DbConnection, DbValue};
use crap_cms_e2e::helpers::*;

// ── version_restore_reverts_doc_to_snapshot ──────────────────────────────

#[tokio::test]
async fn version_restore_reverts_doc_to_snapshot() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_versioned_def(), make_users_def()],
        vec![],
        "verrestore@test.com",
        "pass1234",
    );

    // Create v1: title="Original".
    let _ = post_create_raw(&app, "articles", &cookie, "title=Original&body=v1+body").await;
    let doc_id = only_article_id(&app);

    // Update to v2: title="Modified". Service layer snapshots v1 into
    // _versions_articles before the update lands.
    let _ = post_update_raw(
        &app,
        "articles",
        &doc_id,
        &cookie,
        "title=Modified&body=v2+body",
    )
    .await;

    // Confirm current state shows v2.
    let body = get_edit_form(&app, "articles", &doc_id, &cookie).await;
    assert!(body.contains("Modified"), "edit form should show v2 title");
    assert!(
        !body.contains(r#"value="Original""#),
        "v1 title should be replaced"
    );

    // Find the v1 version_id directly from the versions table.
    let version_id = first_version_id(&app, "articles", &doc_id);

    // Restore v1.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post(format!(
                "/admin/collections/articles/{doc_id}/versions/{version_id}/restore"
            ))
            .header("Cookie", auth_and_csrf(&cookie))
            .header("X-CSRF-Token", TEST_CSRF)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp.status().is_redirection() || resp.status() == StatusCode::OK,
        "restore should succeed, got: {}",
        resp.status()
    );

    // Edit form now shows v1 content again.
    let body = get_edit_form(&app, "articles", &doc_id, &cookie).await;
    assert!(
        body.contains(r#"value="Original""#),
        "edit form should show restored v1 title 'Original', got body snippet around title field"
    );
}

fn make_versioned_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("articles");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Article".to_string())),
        plural: Some(LocalizedString::Plain("Articles".to_string())),
    };
    def.timestamps = true;
    def.versions = Some(VersionsConfig::new(true, 10));
    def.admin = AdminConfig {
        use_as_title: Some("title".to_string()),
        ..AdminConfig::default()
    };
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("body", FieldType::Textarea).build(),
    ];
    def
}

async fn post_create_raw(
    app: &TestApp,
    slug: &str,
    cookie: &str,
    form_body: &str,
) -> (StatusCode, String, Option<String>) {
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post(format!("/admin/collections/{slug}"))
                .header("Cookie", auth_and_csrf(cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(form_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let location = resp
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .or_else(|| {
            resp.headers()
                .get("hx-redirect")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        });
    let body = body_string(resp.into_body()).await;
    (status, body, location)
}

async fn post_update_raw(
    app: &TestApp,
    slug: &str,
    id: &str,
    cookie: &str,
    form_body: &str,
) -> StatusCode {
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post(format!("/admin/collections/{slug}/{id}"))
                .header("Cookie", auth_and_csrf(cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(form_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    resp.status()
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

fn only_article_id(app: &TestApp) -> String {
    let conn = app.pool.get().unwrap();
    let rows = conn
        .query_all("SELECT id FROM articles LIMIT 1", &[])
        .expect("query articles");
    let row = rows.into_iter().next().expect("one article");
    let DbValue::Text(id) = row.get_value(0).cloned().unwrap() else {
        panic!("article id not text")
    };
    id
}

fn first_version_id(app: &TestApp, slug: &str, parent_id: &str) -> String {
    let conn = app.pool.get().unwrap();
    let rows = conn
        .query_all(
            &format!(
                "SELECT id FROM _versions_{slug} WHERE _parent = ?1 ORDER BY _version LIMIT 1"
            ),
            &[DbValue::Text(parent_id.to_string())],
        )
        .expect("query versions");
    let row = rows.into_iter().next().expect("at least one version");
    let DbValue::Text(id) = row.get_value(0).cloned().unwrap() else {
        panic!("version id not text")
    };
    id
}
