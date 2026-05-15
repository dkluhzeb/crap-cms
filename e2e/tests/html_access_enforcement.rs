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

//! Server-side access enforcement — counterpart to `html_access_gating.rs`.
//! The gating file checks that the admin UI hides buttons; this file
//! checks that the server actually rejects forbidden requests even when a
//! user crafts them directly (bypassing the hidden UI). Without this,
//! UI hiding is just defense-in-depth — the real gate must be at the
//! handler.

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

// Lua access functions — same shapes as html_access_gating.rs.

const ACCESS_ADMIN_ONLY: &str = r#"
return function(context)
    return context.user ~= nil and context.user.role == "admin"
end
"#;

const ACCESS_EDITOR_OR_ABOVE: &str = r#"
return function(context)
    if not context.user then return false end
    local role = context.user.role
    return role == "admin" or role == "editor"
end
"#;

const ACCESS_AUTHENTICATED: &str = r"
return function(context)
    return context.user ~= nil
end
";

const ACCESS_NEVER: &str = r"
return function(_context)
    return false
end
";

fn access_files() -> Vec<(&'static str, &'static str)> {
    vec![
        ("admin_only", ACCESS_ADMIN_ONLY),
        ("editor_or_above", ACCESS_EDITOR_OR_ABOVE),
        ("authenticated", ACCESS_AUTHENTICATED),
        ("never", ACCESS_NEVER),
    ]
}

fn make_restricted_posts_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
    ];
    def.access = Access {
        read: Some("access.authenticated".to_string()),
        create: Some("access.editor_or_above".to_string()),
        update: Some("access.editor_or_above".to_string()),
        delete: Some("access.admin_only".to_string()),
        ..Default::default()
    };
    def
}

fn make_no_read_posts_def() -> CollectionDefinition {
    let mut def = make_restricted_posts_def();
    def.access.read = Some("access.never".to_string());
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

// ── viewer_create_post_returns_403 ───────────────────────────────────────
//
// Viewer (no editor/admin role) crafts a POST /admin/collections/posts
// directly, bypassing the hidden UI Create button. Server must reject.

#[tokio::test]
async fn viewer_create_post_returns_403() {
    let app = setup_app_with_access_files(
        vec![make_users_def_with_role(), make_restricted_posts_def()],
        vec![],
        &access_files(),
    );
    let viewer_id = create_test_user_with_role(&app, "v1@test.com", "pw", "viewer");
    let cookie = make_auth_cookie(&app, &viewer_id, "v1@test.com");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/collections/posts")
                .header("Cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=Sneaky"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "viewer must NOT be able to create a post"
    );
}

// ── viewer_update_post_returns_403 ───────────────────────────────────────

#[tokio::test]
async fn viewer_update_post_returns_403() {
    let app = setup_app_with_access_files(
        vec![make_users_def_with_role(), make_restricted_posts_def()],
        vec![],
        &access_files(),
    );
    let viewer_id = create_test_user_with_role(&app, "v2@test.com", "pw", "viewer");
    let cookie = make_auth_cookie(&app, &viewer_id, "v2@test.com");
    let post_id = seed_post(&app, "Existing");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post(format!("/admin/collections/posts/{post_id}"))
                .header("Cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=Modified"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "viewer must NOT update posts"
    );
}

// ── editor_delete_post_returns_403 ───────────────────────────────────────
//
// Editors can update but not delete (delete = admin_only).

#[tokio::test]
async fn editor_delete_post_returns_403() {
    let app = setup_app_with_access_files(
        vec![make_users_def_with_role(), make_restricted_posts_def()],
        vec![],
        &access_files(),
    );
    let editor_id = create_test_user_with_role(&app, "ed@test.com", "pw", "editor");
    let cookie = make_auth_cookie(&app, &editor_id, "ed@test.com");
    let post_id = seed_post(&app, "Important");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::delete(format!("/admin/collections/posts/{post_id}"))
                .header("Cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "editor must NOT delete posts (admin_only)"
    );

    // Doc is still in the DB.
    let conn = app.pool.get().unwrap();
    let rows = conn
        .query_all(
            "SELECT id FROM posts WHERE id = ?1",
            &[DbValue::Text(post_id.clone())],
        )
        .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "post should still exist after rejected delete"
    );
}

// ── admin_delete_post_succeeds ───────────────────────────────────────────
//
// Positive control: admin's identical request DOES succeed. Confirms the
// 403 above is gated on access, not on a generic broken route.

#[tokio::test]
async fn admin_delete_post_succeeds() {
    let app = setup_app_with_access_files(
        vec![make_users_def_with_role(), make_restricted_posts_def()],
        vec![],
        &access_files(),
    );
    let admin_id = create_test_user_with_role(&app, "admin@test.com", "pw", "admin");
    let cookie = make_auth_cookie(&app, &admin_id, "admin@test.com");
    let post_id = seed_post(&app, "Ephemeral");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::delete(format!("/admin/collections/posts/{post_id}"))
                .header("Cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp.status().is_success() || resp.status().is_redirection(),
        "admin delete should succeed, got: {}",
        resp.status()
    );
}

// ── no_read_access_blocks_item_get ───────────────────────────────────────
//
// `read` access fn returns false universally → GET item must NOT leak
// document data.

#[tokio::test]
async fn no_read_access_blocks_item_get() {
    let app = setup_app_with_access_files(
        vec![make_users_def_with_role(), make_no_read_posts_def()],
        vec![],
        &access_files(),
    );
    let viewer_id = create_test_user_with_role(&app, "noread@test.com", "pw", "viewer");
    let cookie = make_auth_cookie(&app, &viewer_id, "noread@test.com");
    let post_id = seed_post(&app, "Secret Title");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!("/admin/collections/posts/{post_id}"))
                .header("Cookie", auth_and_csrf(&cookie))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = body_string(resp.into_body()).await;
    assert!(
        status == StatusCode::FORBIDDEN
            || status == StatusCode::NOT_FOUND
            || !body.contains("Secret Title"),
        "no-read viewer should not see 'Secret Title' in response, got status {status} with body containing it"
    );
}

// ── unauthenticated_post_redirects_or_403 ────────────────────────────────
//
// No session cookie → server must not honor a privileged request.

#[tokio::test]
async fn unauthenticated_post_returns_unauthorized() {
    let app = setup_app_with_access_files(
        vec![make_users_def_with_role(), make_restricted_posts_def()],
        vec![],
        &access_files(),
    );

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/collections/posts")
                .header("Cookie", format!("crap_csrf={TEST_CSRF}"))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=NoAuth"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp.status() == StatusCode::FORBIDDEN
            || resp.status() == StatusCode::UNAUTHORIZED
            || resp.status().is_redirection(),
        "unauthenticated POST should redirect to login or return 403/401, got: {}",
        resp.status()
    );
}
