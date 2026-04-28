use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crap_cms::core::collection::*;
use crap_cms::core::field::*;

use crate::helpers::*;
use crate::html;

// ── Definition builders ──────────────────────────────────────────────────

fn make_versioned_def() -> CollectionDefinition {
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
    def.versions = Some(VersionsConfig::new(true, 10));
    def.admin = AdminConfig {
        use_as_title: Some("title".to_string()),
        ..AdminConfig::default()
    };
    def
}

// ── Local helpers ────────────────────────────────────────────────────────

async fn get_create_form(app: &TestApp, slug: &str, cookie: &str) -> String {
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!("/admin/collections/{slug}/create"))
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    body_string(resp.into_body()).await
}

async fn get_edit_form(app: &TestApp, slug: &str, id: &str, cookie: &str) -> String {
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!("/admin/collections/{slug}/{id}"))
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    body_string(resp.into_body()).await
}

/// POST create and return (status, body, redirect_location)
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
                .header("cookie", auth_and_csrf(cookie))
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
        .or_else(|| resp.headers().get("hx-redirect"))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let body = body_string(resp.into_body()).await;
    (status, body, location)
}

// ── Tests ────────────────────────────────────────────────────────────────

// 1. Versioned create form shows Publish + Save Draft buttons
#[tokio::test]
async fn versioned_create_form_shows_draft_button() {
    let app = setup_app(vec![make_versioned_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "ver1@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "ver1@test.com");

    let body = get_create_form(&app, "articles", &cookie).await;
    let doc = html::parse(&body);

    html::assert_exists(
        &doc,
        "button[value=\"publish\"]",
        "publish button should exist",
    );
    html::assert_exists(
        &doc,
        "button[value=\"save_draft\"]",
        "save draft button should exist",
    );
}

// 1b. The Save-Draft button must carry `formnovalidate` so the browser
// skips constraint validation (notably the HTML `required` attribute)
// when this button submits the form. Without it the user can't save a
// half-filled-in draft because the browser blocks submit before the
// server-side draft-aware validation pipeline runs.
#[tokio::test]
async fn save_draft_button_carries_formnovalidate() {
    use crap_cms::db::query;

    let app = setup_app(vec![make_versioned_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "ver1b@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "ver1b@test.com");

    // Create page (no document yet → uses the create-flow draft button).
    let body = get_create_form(&app, "articles", &cookie).await;
    let doc = html::parse(&body);
    html::assert_exists(
        &doc,
        "button[value=\"save_draft\"][formnovalidate]",
        "save_draft button on the create page must carry formnovalidate",
    );

    // Edit page (existing document → uses the editing-flow draft button).
    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("articles").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let mut data = std::collections::HashMap::new();
    data.insert("title".to_string(), "Edit Me".to_string());
    let created = query::create(&tx, "articles", &def, &data, None).expect("seed doc");
    tx.commit().unwrap();
    drop(conn);

    let body = get_edit_form(&app, "articles", created.id.as_ref(), &cookie).await;
    let doc = html::parse(&body);
    html::assert_exists(
        &doc,
        "button[value=\"save_draft\"][formnovalidate]",
        "save_draft button on the edit page must carry formnovalidate",
    );
}

// 2. Non-versioned form has no draft buttons
#[tokio::test]
async fn non_versioned_form_no_draft_button() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "ver2@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "ver2@test.com");

    let body = get_create_form(&app, "posts", &cookie).await;

    assert!(
        !body.contains("value=\"save_draft\""),
        "non-versioned form should not have save_draft button"
    );
}

// 3. Create as draft skips required validation
#[tokio::test]
async fn create_as_draft_skips_required_validation() {
    let app = setup_app(vec![make_versioned_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "ver3@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "ver3@test.com");

    let (status, _body, location) = post_create_raw(
        &app,
        "articles",
        &cookie,
        "title=&body=Draft+Content&_action=save_draft",
    )
    .await;

    // Draft save should succeed — either a 303 redirect or a 200 with HX-Redirect header
    if status == StatusCode::OK {
        assert!(
            location.is_some(),
            "save_draft with empty required field should succeed (HX-Redirect), but got 200 with no redirect"
        );
    }
}

// 4. Create as published validates required fields
#[tokio::test]
async fn create_as_published_validates_required() {
    let app = setup_app(vec![make_versioned_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "ver4@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "ver4@test.com");

    let (status, body, _location) = post_create_raw(
        &app,
        "articles",
        &cookie,
        "title=&body=Content&_action=publish",
    )
    .await;

    assert_eq!(
        status,
        StatusCode::OK,
        "publish with empty required field should re-render form"
    );
    let doc = html::parse(&body);
    html::assert_exists(&doc, ".form__error", "form should have validation error");
}

// 5. Publish then edit shows published status
#[tokio::test]
async fn publish_then_edit_shows_published_status() {
    let app = setup_app(vec![make_versioned_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "ver5@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "ver5@test.com");

    // Create as published
    let (status, _body, location) = post_create_raw(
        &app,
        "articles",
        &cookie,
        "title=Article&body=Text&_action=publish",
    )
    .await;
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "publish should succeed, got {status}"
    );

    // Find the created doc by listing
    let list_resp = app
        .router
        .clone()
        .oneshot(
            Request::get(location.as_deref().unwrap_or("/admin/collections/articles"))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list_resp.status(), StatusCode::OK);
    let list_body = body_string(list_resp.into_body()).await;
    let list_doc = html::parse(&list_body);
    let edit_link = html::select_one(&list_doc, "table tbody tr a[href]");
    let href = edit_link.value().attr("href").unwrap();

    // GET edit page
    let body = get_edit_form(&app, "articles", href.rsplit('/').next().unwrap(), &cookie).await;

    assert!(
        body.contains("published"),
        "edit page should contain 'published' status"
    );
}

// 6. Edit form shows version sidebar
#[tokio::test]
async fn edit_form_shows_version_sidebar() {
    let app = setup_app(vec![make_versioned_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "ver6@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "ver6@test.com");

    // Create a doc via query::create
    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("articles").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([
        ("title".to_string(), "Versioned Article".to_string()),
        ("body".to_string(), "Some body".to_string()),
    ]);
    let doc_record = crap_cms::db::query::create(&tx, "articles", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let body = get_edit_form(&app, "articles", &doc_record.id, &cookie).await;

    // Version sidebar: the template renders {{t "version_history"}} → "Version History"
    // and the versions URL link is always present
    assert!(
        body.contains("Version History") || body.contains("/versions"),
        "edit page should contain version sidebar"
    );
}

// 7. Update creates version entry
#[tokio::test]
async fn update_creates_version_entry() {
    let app = setup_app(vec![make_versioned_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "ver7@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "ver7@test.com");

    // Create via POST
    let (status, _body, _location) = post_create_raw(
        &app,
        "articles",
        &cookie,
        "title=Original&body=Body&_action=publish",
    )
    .await;
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "create should succeed"
    );

    // Find the doc ID from the list page
    let list_resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/collections/articles")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list_body = body_string(list_resp.into_body()).await;
    let list_doc = html::parse(&list_body);
    let edit_link = html::select_one(&list_doc, "table tbody tr a[href]");
    let href = edit_link.value().attr("href").unwrap();
    let doc_id = href.rsplit('/').next().unwrap();

    // Update via POST
    let update_resp = app
        .router
        .clone()
        .oneshot(
            Request::post(format!("/admin/collections/articles/{doc_id}"))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(
                    "title=Updated&body=New+Body&_action=publish&_method=PUT",
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        update_resp.status() == StatusCode::SEE_OTHER || update_resp.status() == StatusCode::OK,
        "update should succeed"
    );

    // GET versions page
    let ver_resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!("/admin/collections/articles/{doc_id}/versions"))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ver_resp.status(), StatusCode::OK);
    let ver_body = body_string(ver_resp.into_body()).await;
    assert!(
        ver_body.contains("version") || ver_body.contains("v1") || ver_body.contains("v2"),
        "versions page should contain version info"
    );
}

// 8. Unpublish changes status
#[tokio::test]
async fn unpublish_changes_status() {
    let app = setup_app(vec![make_versioned_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "ver8@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "ver8@test.com");

    // Create published doc
    let (status, _body, _location) = post_create_raw(
        &app,
        "articles",
        &cookie,
        "title=Published+Article&body=Body&_action=publish",
    )
    .await;
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "create should succeed"
    );

    // Find doc ID
    let list_resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/collections/articles")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list_body = body_string(list_resp.into_body()).await;
    let list_doc = html::parse(&list_body);
    let edit_link = html::select_one(&list_doc, "table tbody tr a[href]");
    let href = edit_link.value().attr("href").unwrap();
    let doc_id = href.rsplit('/').next().unwrap();

    // Unpublish via POST with _action=unpublish
    let unpublish_resp = app
        .router
        .clone()
        .oneshot(
            Request::post(format!("/admin/collections/articles/{doc_id}"))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(
                    "title=Published+Article&body=Body&_action=unpublish&_method=PUT",
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        unpublish_resp.status() == StatusCode::SEE_OTHER
            || unpublish_resp.status() == StatusCode::OK,
        "unpublish should succeed"
    );

    // GET edit page and verify draft status
    let body = get_edit_form(&app, "articles", doc_id, &cookie).await;
    assert!(
        body.contains("draft"),
        "edit page should show draft status after unpublish"
    );
}
