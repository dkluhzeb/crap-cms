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
use serde_json::{Value, json};
use tower::ServiceExt;

use crap_cms::core::DocumentFields;
use crap_cms::core::HookRef;
use crap_cms::core::collection::*;
use crap_cms::core::field::*;
use crap_cms::db::{DbConnection, DbValue, query};
use crap_cms_e2e::{helpers::*, html};

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

/// Data-aware: a level whose stored `locked` is true is unreadable.
const ACCESS_UNLESS_LOCKED: &str = r"
return function(context)
    return not (context.data and context.data.locked)
end
";

/// A row filter: only posts titled "Mine".
const ACCESS_TITLED_MINE: &str = r#"
return function(_context)
    return { title = "Mine" }
end
"#;

fn access_files() -> Vec<(&'static str, &'static str)> {
    vec![
        ("admin_only", ACCESS_ADMIN_ONLY),
        ("editor_or_above", ACCESS_EDITOR_OR_ABOVE),
        ("authenticated", ACCESS_AUTHENTICATED),
        ("never", ACCESS_NEVER),
        ("titled_mine", ACCESS_TITLED_MINE),
        ("unless_locked", ACCESS_UNLESS_LOCKED),
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
        read: Some(HookRef::new("access.authenticated")),
        create: Some(HookRef::new("access.editor_or_above")),
        update: Some(HookRef::new("access.editor_or_above")),
        delete: Some(HookRef::new("access.admin_only")),
        ..Default::default()
    };
    def
}

fn make_no_read_posts_def() -> CollectionDefinition {
    let mut def = make_restricted_posts_def();
    def.access.read = Some(HookRef::new("access.never"));
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

// ── unreadable_reference_keeps_its_stored_id ─────────────────────────────
//
// Regression: a relationship whose target the viewer may not read rendered
// with no selection, so the form submitted an empty value and saving the
// document silently cleared the stored reference — for a read-only field as
// much as an editable one. The stored id is kept as an "unavailable" item
// (no label, no title leak) and submitted back unchanged.

fn make_secrets_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("secrets");
    def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];
    def.admin.use_as_title = Some("title".to_string());
    def.access.read = Some(HookRef::new("access.never"));
    def
}

fn make_articles_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("articles");
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text).build(),
        FieldDefinition::builder("secret", FieldType::Relationship)
            .relationship(RelationshipConfig::new("secrets", false))
            .build(),
        FieldDefinition::builder("locked", FieldType::Relationship)
            .relationship(RelationshipConfig::new("secrets", false))
            .admin(FieldAdmin::builder().readonly(true).build())
            .build(),
    ];
    def.access = Access {
        read: Some(HookRef::new("access.authenticated")),
        update: Some(HookRef::new("access.authenticated")),
        ..Default::default()
    };
    def
}

fn seed_doc(app: &TestApp, slug: &str, data: Value) -> String {
    let def = app.registry.get_collection(slug).unwrap().clone();
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let fields: DocumentFields = serde_json::from_value(data).unwrap();
    let doc = query::create(&tx, slug, &def, &fields, None).unwrap();
    query::save_join_table_data(&tx, slug, &def.fields, &doc.id, &fields, None).unwrap();
    tx.commit().unwrap();
    doc.id.to_string()
}

#[tokio::test]
async fn unreadable_reference_keeps_its_stored_id() {
    let app = setup_app_with_access_files(
        vec![
            make_users_def_with_role(),
            make_secrets_def(),
            make_articles_def(),
        ],
        vec![],
        &access_files(),
    );
    let viewer_id = create_test_user_with_role(&app, "ref@test.com", "pw", "viewer");
    let cookie = make_auth_cookie(&app, &viewer_id, "ref@test.com");

    let secret_id = seed_doc(&app, "secrets", json!({ "title": "Classified" }));
    let article_id = seed_doc(
        &app,
        "articles",
        json!({ "title": "A", "secret": secret_id, "locked": secret_id }),
    );

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!("/admin/collections/articles/{article_id}"))
                .header("Cookie", auth_and_csrf(&cookie))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(!body.contains("Classified"), "the target's title leaked");

    let doc = html::parse(&body);
    for field in ["secret", "locked"] {
        let host = html::select_one(
            &doc,
            &format!("crap-relationship-search[field-name=\"{field}\"]"),
        );
        let selected: Value =
            serde_json::from_str(host.value().attr("selected").expect("selected attr")).unwrap();

        assert_eq!(selected[0]["id"], secret_id.as_str(), "{field}: {selected}");
        assert_eq!(selected[0]["unavailable"], true, "{field}: {selected}");
    }
}

// ── delete_confirm_judges_a_row_filter_against_the_item ─────────────────
//
// Regression: the delete confirmation page read a filter-table `delete` rule
// as "allowed" for every item, offering a delete the service then refused.
// It is judged against the item, as the delete judges it.

#[tokio::test]
async fn delete_confirm_judges_a_row_filter_against_the_item() {
    let mut posts = make_restricted_posts_def();
    posts.access.delete = Some(HookRef::new("access.titled_mine"));

    let app = setup_app_with_access_files(
        vec![make_users_def_with_role(), posts],
        vec![],
        &access_files(),
    );
    let editor_id = create_test_user_with_role(&app, "rows@test.com", "pw", "editor");
    let cookie = make_auth_cookie(&app, &editor_id, "rows@test.com");
    let mine = seed_post(&app, "Mine");
    let theirs = seed_post(&app, "Theirs");

    let confirm = |id: String| {
        let router = app.router.clone();
        let cookie = cookie.clone();

        async move {
            router
                .oneshot(
                    Request::get(format!("/admin/collections/posts/{id}/delete"))
                        .header("Cookie", cookie)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
                .status()
        }
    };

    assert_eq!(confirm(mine).await, StatusCode::OK);
    assert_eq!(confirm(theirs).await, StatusCode::FORBIDDEN);
}

// ── saving_the_edit_form_keeps_what_the_editor_cannot_read ──────────────
//
// Regression: field `access.read` and `access.update` are independent, and
// the edit form of a user who may update but not read a field rendered an
// EMPTY input for a nested one and submitted an unchecked box / empty list
// for the rest — saving without touching anything flipped a checkbox to
// false, cleared a has-many list and blanked a group sub-field. The form now
// renders no input for an unreadable field at any depth, and the write keeps
// every value its writer cannot read.

fn read_admin_only(builder: FieldDefinitionBuilder) -> FieldDefinition {
    builder
        .access(FieldAccess {
            read: Some(HookRef::new("access.admin_only")),
            ..Default::default()
        })
        .build()
}

fn make_notes_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("notes");
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text).build(),
        read_admin_only(FieldDefinition::builder("verified", FieldType::Checkbox)),
        read_admin_only(
            FieldDefinition::builder("tags", FieldType::Select)
                .has_many(true)
                .options(vec![
                    SelectOption::new(LocalizedString::Plain("A".into()), "a"),
                    SelectOption::new(LocalizedString::Plain("B".into()), "b"),
                ]),
        ),
        FieldDefinition::builder("internal", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("label", FieldType::Text).build(),
                read_admin_only(FieldDefinition::builder("note", FieldType::Text)),
            ])
            .build(),
    ];
    def.access = Access {
        read: Some(HookRef::new("access.authenticated")),
        update: Some(HookRef::new("access.authenticated")),
        ..Default::default()
    };
    def
}

#[tokio::test]
async fn saving_the_edit_form_keeps_what_the_editor_cannot_read() {
    let app = setup_app_with_access_files(
        vec![make_users_def_with_role(), make_notes_def()],
        vec![],
        &access_files(),
    );
    let editor_id = create_test_user_with_role(&app, "notes@test.com", "pw", "editor");
    let cookie = make_auth_cookie(&app, &editor_id, "notes@test.com");

    let id = seed_doc(
        &app,
        "notes",
        json!({
            "title": "Old",
            "verified": true,
            "tags": ["a"],
            "internal": { "label": "l", "note": "secret" },
        }),
    );

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!("/admin/collections/notes/{id}"))
                .header("Cookie", auth_and_csrf(&cookie))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let doc = html::parse(&body);

    for name in ["verified", "tags", "internal__note"] {
        assert!(
            html::select_all(&doc, &format!("[name=\"{name}\"]")).is_empty(),
            "no input renders for the unreadable `{name}`"
        );
    }
    assert!(!body.contains("secret"), "the unreadable value leaked");

    // What the form submits: only the inputs it rendered.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post(format!("/admin/collections/notes/{id}"))
                .header("Cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=New&internal__label=m"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp.status().is_success() || resp.status().is_redirection(),
        "the save lands: {}",
        resp.status()
    );

    let def = app.registry.get_collection("notes").unwrap().clone();
    let conn = app.pool.get().unwrap();
    let stored = query::find_by_id(&conn, "notes", &def, &id, None)
        .unwrap()
        .expect("the note");

    assert_eq!(stored.fields.get("title"), Some(&json!("New")));
    assert_eq!(stored.fields.get("verified"), Some(&json!(true)));
    assert_eq!(stored.fields.get("tags"), Some(&json!(["a"])));

    let internal = stored.fields.get("internal").expect("the group");
    assert_eq!(internal["label"], "m");
    assert_eq!(internal["note"], "secret");
}

// ── a_row_field_is_editable_where_its_row_lets_the_editor_read_it ───────
//
// Regression: a data-aware read rule on an array sub-field denies it in some
// rows only, but the edit form judged it once for the whole document, so
// every row rendered alike — the field dropped from every row (editable
// nowhere) or an input in every row, empty where the value was hidden. The
// rule is judged row by row, as the read strip judges it: a row the editor
// may read renders and saves the field, a row it may not read renders no
// input and keeps its stored value, and the new-row template offers it (a new
// row is judged against an empty one).

fn make_tasks_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("tasks");
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text).build(),
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("locked", FieldType::Checkbox).build(),
                FieldDefinition::builder("done", FieldType::Checkbox)
                    .access(FieldAccess {
                        read: Some(HookRef::new("access.unless_locked")),
                        ..Default::default()
                    })
                    .build(),
            ])
            .build(),
    ];
    def.access = Access {
        read: Some(HookRef::new("access.authenticated")),
        update: Some(HookRef::new("access.authenticated")),
        ..Default::default()
    };
    def
}

#[tokio::test]
async fn a_row_field_is_editable_where_its_row_lets_the_editor_read_it() {
    let app = setup_app_with_access_files(
        vec![make_users_def_with_role(), make_tasks_def()],
        vec![],
        &access_files(),
    );
    let editor_id = create_test_user_with_role(&app, "tasks@test.com", "pw", "editor");
    let cookie = make_auth_cookie(&app, &editor_id, "tasks@test.com");

    let id = seed_doc(
        &app,
        "tasks",
        json!({
            "title": "T",
            "items": [
                { "locked": true, "done": true },
                { "locked": false, "done": false },
            ],
        }),
    );

    let def = app.registry.get_collection("tasks").unwrap().clone();
    let stored_items = |app: &TestApp| {
        let conn = app.pool.get().unwrap();
        query::find_by_id(&conn, "tasks", &def, &id, None)
            .unwrap()
            .expect("the task")
            .fields
            .get("items")
            .cloned()
            .expect("the rows")
    };
    let rows = stored_items(&app);
    let (first, second) = (
        rows[0]["id"].as_str().unwrap().to_string(),
        rows[1]["id"].as_str().unwrap().to_string(),
    );

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!("/admin/collections/tasks/{id}"))
                .header("Cookie", auth_and_csrf(&cookie))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let page = html::parse(&body);
    assert!(
        html::select_all(&page, "[name=\"items[0][done]\"]").is_empty(),
        "the locked row renders no input for a value its editor cannot read"
    );
    assert!(
        !html::select_all(&page, "[name=\"items[1][done]\"]").is_empty(),
        "the unlocked row renders the field its editor may read"
    );
    assert!(
        body.contains("items[__INDEX__][done]"),
        "a new row offers the field: an empty row is not locked"
    );

    // What the form submits: each row's id, its rendered `locked` box, and
    // the unlocked row's `done` box — now checked.
    let form = format!(
        "title=T&items%5B0%5D%5Bid%5D={first}&items%5B0%5D%5Blocked%5D=on\
         &items%5B1%5D%5Bid%5D={second}&items%5B1%5D%5Bdone%5D=on"
    );
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post(format!("/admin/collections/tasks/{id}"))
                .header("Cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(form))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp.status().is_success() || resp.status().is_redirection(),
        "the save lands: {}",
        resp.status()
    );

    let rows = stored_items(&app);
    assert_eq!(rows[0]["done"], json!(true), "the unreadable row keeps it");
    assert_eq!(
        rows[1]["done"],
        json!(true),
        "the readable row saves the edit: {rows}"
    );
}
