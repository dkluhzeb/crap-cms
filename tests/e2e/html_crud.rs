use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;
use tower::ServiceExt;

use crap_cms::core::DocumentFields;
use crap_cms::core::collection::*;
use crap_cms::core::field::*;
use crap_cms::db::query;

use crate::helpers::*;
use crate::html;

fn make_crud_def() -> CollectionDefinition {
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
        FieldDefinition::builder("body", FieldType::Textarea).build(),
    ];
    def.admin = AdminConfig {
        use_as_title: Some("title".to_string()),
        ..AdminConfig::default()
    };
    def
}

// ── 17. create_redirects_to_edit_with_data ────────────────────────────────

#[tokio::test]
async fn create_redirects_to_edit_with_data() {
    let app = setup_app(vec![make_crud_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "crud@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "crud@test.com");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/collections/posts")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=New+Post&body=Hello"))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "create should redirect or HX-Redirect, got {status}"
    );

    // Create action redirects to the collection list page
    let location = if status == StatusCode::SEE_OTHER {
        resp.headers()
            .get("location")
            .map(|v| v.to_str().unwrap().to_string())
    } else {
        resp.headers()
            .get("hx-redirect")
            .map(|v| v.to_str().unwrap().to_string())
    };

    if let Some(loc) = location {
        assert!(
            loc.contains("/admin/collections/posts"),
            "redirect should point to collection, got {loc}"
        );

        // Follow the redirect to the list page
        let resp = app
            .router
            .clone()
            .oneshot(
                Request::get(&loc)
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp.into_body()).await;

        // The created document should appear on the list page
        assert!(
            body.contains("New Post"),
            "list page should contain the created document"
        );

        // Find edit link and verify data in edit form
        let doc = html::parse(&body);
        let link = html::select_one(&doc, "table tbody tr a[href]");
        let href = link.value().attr("href").unwrap();

        let resp = app
            .router
            .clone()
            .oneshot(
                Request::get(href)
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp.into_body()).await;
        let doc = html::parse(&body);

        html::assert_input(&doc, "title", "text", Some("New Post"));
    }
}

// ── 18. update_redirects_with_updated_data ────────────────────────────────

#[tokio::test]
async fn update_redirects_with_updated_data() {
    let app = setup_app(vec![make_crud_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "update@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "update@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields =
        std::collections::HashMap::from([("title".to_string(), json!("Original Title"))]).into();
    let doc_record = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post(format!("/admin/collections/posts/{}", doc_record.id))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=Updated+Title&body=New+Body"))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "update should redirect or HX-Redirect, got {status}"
    );

    // GET the edit page to verify updated data
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!("/admin/collections/posts/{}", doc_record.id))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let doc = html::parse(&body);

    html::assert_input(&doc, "title", "text", Some("Updated Title"));
}

// ── 19. delete_removes_from_list ──────────────────────────────────────────

#[tokio::test]
async fn delete_removes_from_list() {
    let app = setup_app(vec![make_crud_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "delete@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "delete@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields =
        std::collections::HashMap::from([("title".to_string(), json!("Delete Me Please"))]).into();
    let doc_record = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();

    // Delete
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::delete(format!("/admin/collections/posts/{}", doc_record.id))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "delete should redirect or HX-Redirect, got {status}"
    );

    // GET list and verify the doc is gone
    let resp = app
        .router
        .clone()
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
        !body.contains("Delete Me Please"),
        "list page should not contain the deleted document's title"
    );
}

// ── 20. list_page_shows_documents ─────────────────────────────────────────

#[tokio::test]
async fn list_page_shows_documents() {
    let app = setup_app(vec![make_crud_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "list@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "list@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    for title in &["Alpha Post", "Beta Post", "Gamma Post"] {
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let data: DocumentFields =
            std::collections::HashMap::from([("title".to_string(), json!(title))]).into();
        query::create(&tx, "posts", &def, &data, None).unwrap();
        tx.commit().unwrap();
    }

    let resp = app
        .router
        .clone()
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
        body.contains("Alpha Post"),
        "list should contain Alpha Post"
    );
    assert!(body.contains("Beta Post"), "list should contain Beta Post");
    assert!(
        body.contains("Gamma Post"),
        "list should contain Gamma Post"
    );

    // Also verify with HTML parsing: table rows exist
    let doc = html::parse(&body);
    let rows = html::count(&doc, "table tbody tr");
    assert!(
        rows >= 3,
        "list table should have at least 3 rows, got {rows}"
    );
}
