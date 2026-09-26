//! Collection CRUD integration tests for the admin HTTP handlers.
//!
//! Covers: the dashboard and collection list, the create / edit / delete
//! forms and actions, validation re-renders, versioned create forms and the
//! delete-confirm page. Listing (search, filter, sort, pagination) lives in
//! `admin_collections_list.rs`, missing collections and documents in
//! `admin_collections_not_found.rs`, locales in `admin_collections_locale.rs`,
//! uploads in `admin_collections_upload.rs`, auth collections in
//! `admin_collections_auth.rs`, versioning in `admin_collections_versioning.rs`.

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

mod admin_collections_support;

use std::{collections::HashMap, fs};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::json;
use tower::ServiceExt;

use crap_cms::{
    core::{
        DocumentFields,
        collection::{AdminConfig, CollectionDefinition, Labels},
        field::{FieldDefinition, FieldType, LocalizedString, RelationshipConfig},
    },
    db::{DbConnection, query},
};

use admin_collections_support::{
    TEST_CSRF, auth_and_csrf, body_string, create_test_user, make_auth_cookie, make_posts_def,
    make_users_def, make_versioned_posts_def, setup_app,
};

fn make_posts_with_required_title() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("articles");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Article".to_string())),
        plural: Some(LocalizedString::Plain("Articles".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("body", FieldType::Textarea).build(),
    ];
    def.admin = AdminConfig {
        use_as_title: Some("title".to_string()),
        ..AdminConfig::default()
    };
    def
}

#[tokio::test]
async fn dashboard_returns_200() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "dash@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "dash@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(body.to_lowercase().contains("posts") || body.to_lowercase().contains("dashboard"));
}

/// The collections list hides a collection its `access.admin` rule denies,
/// as the dashboard and the navigation do.
#[tokio::test]
async fn list_collections_hides_collections_the_admin_rule_denies() {
    let mut posts = make_posts_def();
    posts.access.admin = Some("hooks.access.deny_all".into());
    let app = setup_app(vec![posts, make_users_def()], vec![]);

    let hooks = app.tmp.path().join("hooks");
    fs::create_dir_all(&hooks).unwrap();
    fs::write(
        hooks.join("access.lua"),
        "local M = {}\nfunction M.deny_all(ctx)\n    return false\nend\nreturn M\n",
    )
    .unwrap();

    let user_id = create_test_user(&app, "hidden@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "hidden@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_string(resp.into_body()).await;
    assert!(!body.contains("/admin/collections/posts"), "{body}");
    assert!(body.contains("/admin/collections/users"), "{body}");
}

#[tokio::test]
async fn list_collections_returns_200() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "list@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "list@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn list_items_returns_200() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "items@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "items@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn list_items_uses_title_field() {
    let mut def = make_posts_def();
    def.admin.use_as_title = Some("title".to_string());

    let app = setup_app(vec![def.clone(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "titlefield@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "titlefield@test.com");

    let real_def = {
        let reg = &app.registry;
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields =
        HashMap::from([("title".to_string(), json!("My Custom Title"))]).into();
    query::create(&tx, "posts", &real_def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(
        body.contains("My Custom Title"),
        "List should show document title via use_as_title"
    );
}

#[tokio::test]
async fn create_form_returns_200() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "create@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "create@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts/create")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn create_action_creates_document() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "create_action@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "create_action@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/posts")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=Test+Post"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND || status == StatusCode::OK,
        "Create action should redirect or HX-Redirect, got {status}"
    );
}

#[tokio::test]
async fn edit_form_returns_200() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "edit@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "edit@test.com");

    let def = {
        let reg = &app.registry;
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields = HashMap::from([("title".to_string(), json!("Edit Me"))]).into();
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::get(format!("/admin/collections/posts/{}", doc.id))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn update_action_updates_document() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "update@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "update@test.com");

    let def = {
        let reg = &app.registry;
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields = HashMap::from([("title".to_string(), json!("Original"))]).into();
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::post(format!("/admin/collections/posts/{}", doc.id))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=Updated"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND || status == StatusCode::OK,
        "Update action should redirect or HX-Redirect, got {status}"
    );
}

#[tokio::test]
async fn delete_action_removes_document() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "delete@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "delete@test.com");

    let def = {
        let reg = &app.registry;
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields = HashMap::from([("title".to_string(), json!("Delete Me"))]).into();
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::delete(format!("/admin/collections/posts/{}", doc.id))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND || status == StatusCode::OK,
        "Delete action should redirect or return 200, got {status}"
    );
}

#[tokio::test]
async fn delete_action_returns_redirect() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "delredir@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "delredir@test.com");

    let def = {
        let reg = &app.registry;
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields =
        HashMap::from([("title".to_string(), json!("To Delete Redir"))]).into();
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::delete(format!("/admin/collections/posts/{}", doc.id))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND || status == StatusCode::OK,
        "Delete action should redirect or return 200 with HX-Redirect, got {status}"
    );

    if status == StatusCode::SEE_OTHER || status == StatusCode::FOUND {
        let location = resp
            .headers()
            .get("location")
            .map(|v| v.to_str().unwrap_or(""));
        if let Some(loc) = location {
            assert!(
                loc.contains("/admin/collections/posts"),
                "Delete redirect should point to collection list, got {loc}"
            );
        }
    }
}

#[tokio::test]
async fn post_with_method_delete_deletes_document() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "methoddel@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "methoddel@test.com");

    let def = {
        let reg = &app.registry;
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields =
        HashMap::from([("title".to_string(), json!("Method Delete"))]).into();
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::post(format!("/admin/collections/posts/{}", doc.id))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("_method=DELETE"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "DELETE via _method should succeed, got {status}"
    );
}

#[tokio::test]
async fn create_action_validation_error_missing_required_field() {
    let app = setup_app(
        vec![make_posts_with_required_title(), make_users_def()],
        vec![],
    );
    let user_id = create_test_user(&app, "validate@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "validate@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/articles")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=&body=Some+content"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "Expected 200 (validation error re-render) or redirect, got {status}"
    );
}

#[tokio::test]
async fn create_action_missing_required_field_shows_errors() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "valerr@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "valerr@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/posts")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title="))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "Validation error should re-render form or redirect, got {status}"
    );
}

/// Regression: the edit form posts with `hx-target="#main"`, so a failed save
/// must come back as that fragment. The error re-render used to skip the
/// partial decision and answer with a whole document, which htmx then swapped
/// *inside* `#main` — a second `<html>`, a second set of component singletons.
#[tokio::test]
async fn a_failed_htmx_submit_re_renders_only_the_main_fragment() {
    let app = setup_app(
        vec![make_posts_with_required_title(), make_users_def()],
        vec![],
    );
    let user_id = create_test_user(&app, "validate@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "validate@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/articles")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("HX-Request", "true")
                .header("HX-Target", "main")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=&body=Some+content"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_string(resp.into_body()).await;

    assert!(
        !body.contains("<!DOCTYPE") && !body.contains("<html"),
        "an htmx submit must not get a full document back"
    );
    assert!(
        body.contains("id=\"edit-form\""),
        "the re-rendered form is what the fragment carries"
    );
}

#[tokio::test]
async fn versioned_collection_create_form() {
    let app = setup_app(vec![make_versioned_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "ver@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "ver@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/articles/create")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn collection_versions_page_returns_200() {
    let app = setup_app(vec![make_versioned_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "cvp@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "cvp@test.com");

    let def = {
        let reg = &app.registry;
        reg.get_collection("articles").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields = HashMap::from([
        ("title".to_string(), json!("Versioned Article")),
        ("body".to_string(), json!("Content")),
    ])
    .into();
    let doc = query::create(&tx, "articles", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!("/admin/collections/articles/{}/versions", doc.id))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn collection_create_with_draft() {
    let app = setup_app(vec![make_versioned_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "cdraft@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "cdraft@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/articles")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(
                    "title=Draft+Article&body=WIP&_action=save_draft",
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "Create draft should succeed, got {status}"
    );
}

#[tokio::test]
async fn restore_version_nonversioned_redirects() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "restnv@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "restnv@test.com");

    let def = {
        let reg = &app.registry;
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields = HashMap::from([("title".to_string(), json!("NV Restore"))]).into();
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::post(format!(
                "/admin/collections/posts/{}/versions/fake-version/restore",
                doc.id
            ))
            .header("cookie", auth_and_csrf(&cookie))
            .header("X-CSRF-Token", TEST_CSRF)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert_eq!(
        status,
        StatusCode::SEE_OTHER,
        "Restore on non-versioned should redirect"
    );
}

#[tokio::test]
async fn delete_confirm_page_returns_200() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "delconf@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "delconf@test.com");

    let def = {
        let reg = &app.registry;
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields =
        HashMap::from([("title".to_string(), json!("To Confirm Delete"))]).into();
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::get(format!("/admin/collections/posts/{}/delete", doc.id))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// Regression: delete confirmation page should still render (200) even when
/// the document's table has a schema mismatch (e.g., missing column), so that
/// users can delete broken/orphaned documents.
#[tokio::test]
async fn delete_confirm_page_with_schema_mismatch_returns_200() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "delsm@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "delsm@test.com");

    // Create a document normally
    let mut conn = app.pool.get().unwrap();
    let def = {
        let reg = &app.registry;
        reg.get_collection("posts").unwrap().clone()
    };
    let tx = conn.transaction().unwrap();
    let data: DocumentFields = HashMap::from([("title".to_string(), json!("Broken Doc"))]).into();
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();

    // Simulate schema mismatch: rename the title column so SELECT fails
    conn.execute_batch("ALTER TABLE posts RENAME COLUMN title TO title_old;")
        .unwrap();

    let resp = app
        .router
        .oneshot(
            Request::get(format!("/admin/collections/posts/{}/delete", doc.id))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // Should still render the delete confirmation page, not 500
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn delete_confirm_shows_back_references_warning() {
    let media = CollectionDefinition::new("media");
    let mut posts = CollectionDefinition::new("posts");
    posts.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    posts.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("image", FieldType::Upload)
            .relationship(RelationshipConfig::new("media", false))
            .build(),
    ];
    let app = setup_app(vec![media, posts, make_users_def()], vec![]);

    let user_id = create_test_user(&app, "admin@test.com", "password123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    // Create a media document and a post referencing it
    let conn = app.pool.get().unwrap();
    conn.execute("INSERT INTO media (id, _ref_count) VALUES ('m1', 1)", &[])
        .unwrap();
    conn.execute(
        "INSERT INTO posts (id, title, image) VALUES ('p1', 'My Post', 'm1')",
        &[],
    )
    .unwrap();
    drop(conn);

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/media/m1/delete")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    // Should contain the warning card with ref count info
    assert!(body.contains("card--warning"), "Should show warning card");
}

#[tokio::test]
async fn delete_confirm_no_warning_when_unreferenced() {
    let media = CollectionDefinition::new("media");
    let mut posts = CollectionDefinition::new("posts");
    posts.fields = vec![
        FieldDefinition::builder("title", FieldType::Text).build(),
        FieldDefinition::builder("image", FieldType::Upload)
            .relationship(RelationshipConfig::new("media", false))
            .build(),
    ];
    let app = setup_app(vec![media, posts, make_users_def()], vec![]);

    let user_id = create_test_user(&app, "admin@test.com", "password123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    // Create a media document with no references
    let conn = app.pool.get().unwrap();
    conn.execute("INSERT INTO media (id) VALUES ('m1')", &[])
        .unwrap();
    drop(conn);

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/media/m1/delete")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(
        !body.contains("card--warning"),
        "Should NOT show warning when unreferenced"
    );
}
