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
//! HTML access-gating tests — verify the admin UI hides action buttons
//! that the current user isn't allowed to use.
//!
//! Server-side enforcement is exercised in the role-based access tests
//! elsewhere; this file covers the *template-level* gates added to keep
//! users from seeing buttons that would always 403.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crap_cms_e2e::helpers::*;
use crap_cms_e2e::html;

use crap_cms::core::collection::*;
use crap_cms::core::field::*;

// ── Lua access fns used in these tests ───────────────────────────────────
//
// Each test config dir gets these written into `access/`. The fns mirror
// the example access setup: `admin_only` and `editor_or_above` gate on
// `context.user.role`.

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

/// Build a `posts` collection with editor-restricted create/update and
/// admin-only delete — matches the shape of the example's `pages` /
/// `posts` collections.
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

/// Build a `settings` global with admin-only update — matches the
/// `footer` / `navigation` globals from the example.
fn make_restricted_settings_def() -> GlobalDefinition {
    let mut def = GlobalDefinition::new("settings");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Settings".to_string())),
        plural: None,
    };
    def.fields = vec![FieldDefinition::builder("site_name", FieldType::Text).build()];
    def.access = Access {
        read: Some("access.authenticated".to_string()),
        update: Some("access.admin_only".to_string()),
        ..Default::default()
    };
    def
}

fn standard_access_files() -> Vec<(&'static str, &'static str)> {
    vec![
        ("admin_only", ACCESS_ADMIN_ONLY),
        ("editor_or_above", ACCESS_EDITOR_OR_ABOVE),
        ("authenticated", ACCESS_AUTHENTICATED),
    ]
}

// ── 1. List page: create button gated on per-user create access ─────────

#[tokio::test]
async fn list_page_hides_create_button_when_user_lacks_create_access() {
    let app = setup_app_with_access_files(
        vec![make_users_def_with_role(), make_restricted_posts_def()],
        vec![],
        &standard_access_files(),
    );

    // A "viewer" user has neither editor nor admin role — no create access.
    let viewer_id = create_test_user_with_role(&app, "viewer@test.com", "pw", "viewer");
    let cookie = make_auth_cookie(&app, &viewer_id, "viewer@test.com");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/collections/posts")
                .header("Cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;

    let doc = html::parse(&body);

    // Create button → hidden. Match the partial that renders it.
    html::assert_not_exists(
        &doc,
        "a[href='/admin/collections/posts/create']",
        "viewer should not see the Create button",
    );
}

#[tokio::test]
async fn list_page_shows_create_button_when_user_has_create_access() {
    let app = setup_app_with_access_files(
        vec![make_users_def_with_role(), make_restricted_posts_def()],
        vec![],
        &standard_access_files(),
    );

    let editor_id = create_test_user_with_role(&app, "editor@test.com", "pw", "editor");
    let cookie = make_auth_cookie(&app, &editor_id, "editor@test.com");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/collections/posts")
                .header("Cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_string(resp.into_body()).await;
    let doc = html::parse(&body);

    html::assert_exists(
        &doc,
        "a[href='/admin/collections/posts/create']",
        "editor should see the Create button",
    );
}

// ── 2. Edit page: save panel gated on update access ─────────────────────

#[tokio::test]
async fn edit_page_hides_save_panel_when_user_lacks_update() {
    let app = setup_app_with_access_files(
        vec![make_users_def_with_role(), make_restricted_posts_def()],
        vec![],
        &standard_access_files(),
    );

    // Seed a post as admin (write directly).
    let post_id = {
        use crap_cms::core::DocumentFields;
        use crap_cms::db::query;
        let def = app.registry.get_collection("posts").unwrap().clone();
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let data: DocumentFields =
            std::collections::HashMap::from([("title".to_string(), serde_json::json!("Hello"))])
                .into();
        let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
        tx.commit().unwrap();
        doc.id.to_string()
    };

    // viewer can read (access.authenticated) but cannot update.
    let viewer_id = create_test_user_with_role(&app, "viewer@test.com", "pw", "viewer");
    let cookie = make_auth_cookie(&app, &viewer_id, "viewer@test.com");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!("/admin/collections/posts/{post_id}"))
                .header("Cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_string(resp.into_body()).await;
    let doc = html::parse(&body);

    // No submit button in the edit sidebar — viewer can read but not save.
    html::assert_not_exists(
        &doc,
        ".edit-sidebar button[type='submit']",
        "viewer should not see a Save submit button",
    );
}

#[tokio::test]
async fn edit_page_hides_delete_panel_when_user_lacks_delete() {
    let app = setup_app_with_access_files(
        vec![make_users_def_with_role(), make_restricted_posts_def()],
        vec![],
        &standard_access_files(),
    );

    let post_id = {
        use crap_cms::core::DocumentFields;
        use crap_cms::db::query;
        let def = app.registry.get_collection("posts").unwrap().clone();
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let data: DocumentFields =
            std::collections::HashMap::from([("title".to_string(), serde_json::json!("Hello"))])
                .into();
        let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
        tx.commit().unwrap();
        doc.id.to_string()
    };

    // editor can read/update but not delete (delete is admin-only here).
    let editor_id = create_test_user_with_role(&app, "editor@test.com", "pw", "editor");
    let cookie = make_auth_cookie(&app, &editor_id, "editor@test.com");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!("/admin/collections/posts/{post_id}"))
                .header("Cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_string(resp.into_body()).await;
    let doc = html::parse(&body);

    // Delete button uses `button--danger` in the sidebar.
    html::assert_not_exists(
        &doc,
        ".edit-sidebar button.button--danger",
        "editor should not see the Delete button when delete is admin-only",
    );
}

#[tokio::test]
async fn edit_page_shows_save_and_delete_for_admin() {
    let app = setup_app_with_access_files(
        vec![make_users_def_with_role(), make_restricted_posts_def()],
        vec![],
        &standard_access_files(),
    );

    let post_id = {
        use crap_cms::core::DocumentFields;
        use crap_cms::db::query;
        let def = app.registry.get_collection("posts").unwrap().clone();
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let data: DocumentFields =
            std::collections::HashMap::from([("title".to_string(), serde_json::json!("Hello"))])
                .into();
        let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
        tx.commit().unwrap();
        doc.id.to_string()
    };

    let admin_id = create_test_user_with_role(&app, "admin@test.com", "pw", "admin");
    let cookie = make_auth_cookie(&app, &admin_id, "admin@test.com");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!("/admin/collections/posts/{post_id}"))
                .header("Cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_string(resp.into_body()).await;
    let doc = html::parse(&body);

    html::assert_exists(
        &doc,
        ".edit-sidebar button[type='submit']",
        "admin should see the Save submit button",
    );
    html::assert_exists(
        &doc,
        ".edit-sidebar button.button--danger",
        "admin should see the Delete button",
    );
}

// ── 3. Global edit: save panel gated on update access ───────────────────

#[tokio::test]
async fn global_edit_hides_save_panel_when_user_lacks_update() {
    let app = setup_app_with_access_files(
        vec![make_users_def_with_role()],
        vec![make_restricted_settings_def()],
        &standard_access_files(),
    );

    let editor_id = create_test_user_with_role(&app, "editor@test.com", "pw", "editor");
    let cookie = make_auth_cookie(&app, &editor_id, "editor@test.com");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/globals/settings")
                .header("Cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_string(resp.into_body()).await;
    let doc = html::parse(&body);

    html::assert_not_exists(
        &doc,
        ".edit-sidebar button[type='submit']",
        "editor should not see a Save submit on an admin-only global",
    );
}

#[tokio::test]
async fn global_edit_shows_save_panel_for_admin() {
    let app = setup_app_with_access_files(
        vec![make_users_def_with_role()],
        vec![make_restricted_settings_def()],
        &standard_access_files(),
    );

    let admin_id = create_test_user_with_role(&app, "admin@test.com", "pw", "admin");
    let cookie = make_auth_cookie(&app, &admin_id, "admin@test.com");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/globals/settings")
                .header("Cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_string(resp.into_body()).await;
    let doc = html::parse(&body);

    html::assert_exists(
        &doc,
        ".edit-sidebar button[type='submit']",
        "admin should see the Save submit on an admin-only global",
    );
}

// ── 4. Forbidden response carries X-Crap-Toast ──────────────────────────

#[tokio::test]
async fn forbidden_response_includes_x_crap_toast_for_htmx_consumption() {
    let app = setup_app_with_access_files(
        vec![make_users_def_with_role(), make_restricted_posts_def()],
        vec![],
        &standard_access_files(),
    );

    // viewer doesn't have create access; the create-form route enforces it
    // with `check_access_or_forbid`, which calls `forbidden()` on denial.
    let viewer_id = create_test_user_with_role(&app, "viewer@test.com", "pw", "viewer");
    let cookie = make_auth_cookie(&app, &viewer_id, "viewer@test.com");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/collections/posts/create")
                .header("Cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let toast = resp
        .headers()
        .get("X-Crap-Toast")
        .expect("forbidden() should set X-Crap-Toast so htmx clients can surface a toast");
    let toast_str = toast.to_str().unwrap();
    assert!(
        toast_str.contains("\"type\":\"error\""),
        "toast should carry an error type: got {toast_str:?}"
    );
}
