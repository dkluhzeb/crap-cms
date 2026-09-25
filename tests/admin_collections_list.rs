//! Collection list integration tests for the admin HTTP handlers.
//!
//! Covers: URL filters (status, OR clauses, field filters), search (configured
//! searchable fields, special characters, no results), sorting and
//! pagination of the collection list.

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

use std::collections::HashMap;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::json;
use tower::ServiceExt;

use crap_cms::{
    config::LocaleConfig,
    core::{
        DocumentFields,
        collection::{AdminConfig, CollectionDefinition, Labels, VersionsConfig},
        field::{FieldDefinition, FieldType, LocalizedString, SelectOption},
    },
    db::{DbConnection, DbValue, query},
};

use admin_collections_support::{
    TestApp, body_string, create_test_user, make_auth_cookie, make_posts_def, make_users_def,
    make_versioned_posts_def, setup_app,
};

/// The list page of `slug`, asked to sort by the draft-status column.
async fn status_sorted_list(app: &TestApp, cookie: &str, slug: &str) -> StatusCode {
    app.router
        .clone()
        .oneshot(
            Request::get(format!("/admin/collections/{slug}?sort=_status"))
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

fn make_searchable_posts_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("sposts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Searchable Post".to_string())),
        plural: Some(LocalizedString::Plain("Searchable Posts".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("body", FieldType::Textarea).build(),
        FieldDefinition::builder("category", FieldType::Text).build(),
    ];
    def.admin = AdminConfig {
        use_as_title: Some("title".to_string()),
        list_searchable_fields: vec!["title".to_string(), "body".to_string()],
        ..AdminConfig::default()
    };
    def
}

/// Regression test for the user-reported "list shows all (published)
/// items when filter is set to draft" symptom. `_status` is a system
/// column (`_*` prefix) so it cannot ride the generic user-filter
/// pipeline (`validate_user_filters` rejects `_*`). The admin list
/// handler extracts `?where[_status][equals]=X` from the raw query
/// via `extract_status_filter` and forwards it as a typed
/// `status_filter` on `FindDocumentsInput` so it bypasses
/// validation and reaches SQL via the trusted post-validation
/// injection path in `build_effective_query`.
///
/// This test asserts:
/// - unfiltered shows both draft and published rows;
/// - `?where[_status][equals]=draft` narrows to drafts only;
/// - `?where[_status][equals]=published` narrows to published only.
#[tokio::test]
async fn list_items_url_status_filter_narrows_drafts_only() {
    fn posts_with_drafts_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.timestamps = true;
        def.versions = Some(VersionsConfig::new(true, 10));
        def.admin.use_as_title = Some("title".to_string());
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .required(true)
                .build(),
        ];
        def
    }

    let app = setup_app(vec![posts_with_drafts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "statusf@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "statusf@test.com");

    let def = {
        let reg = &app.registry;
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    // Both rows start `_status='published'` (column default per
    // `migrate/collection/create.rs:131`). Demote one to `draft` via a
    // direct UPDATE — this is what the service-layer
    // `persist::create_document` does internally when `is_draft = true`,
    // we just skip the service wrapper here to keep the test focused on
    // the filter pipeline.
    let mut data1 = DocumentFields::new();
    data1.insert("title".to_string(), json!("Live Article"));
    query::create(&tx, "posts", &def, &data1, None).expect("publish create ok");

    let mut data2 = DocumentFields::new();
    data2.insert("title".to_string(), json!("Pending Draft"));
    let draft_doc = query::create(&tx, "posts", &def, &data2, None).expect("draft create ok");
    tx.execute(
        "UPDATE posts SET _status = 'draft' WHERE id = ?1",
        &[DbValue::Text(draft_doc.id.to_string())],
    )
    .expect("set _status=draft");
    tx.commit().unwrap();
    drop(conn);

    fn count_table_rows(body: &str) -> usize {
        let Some(start) = body.find("<tbody") else {
            return 0;
        };
        let Some(end) = body[start..].find("</tbody>").map(|i| start + i) else {
            return 0;
        };
        body[start..end].matches("<tr").count()
    }

    // Sanity: unfiltered shows both (admin defaults include_drafts=true).
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
    let body = body_string(resp.into_body()).await;
    assert_eq!(
        count_table_rows(&body),
        2,
        "unfiltered admin list should show both draft and published"
    );

    // Filter to drafts only via `?where[_status][equals]=draft`. URL-encoded
    // brackets — what the browser sends from the filter drawer.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/collections/posts?where%5B_status%5D%5Bequals%5D=draft")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert_eq!(
        count_table_rows(&body),
        1,
        "?where[_status][equals]=draft should narrow to 1 draft row"
    );
    assert!(body.contains("Pending Draft"));
    assert!(
        !body.contains("Live Article"),
        "draft filter must NOT include the published doc — typed-param plumbing regression"
    );

    // Symmetric: filter to published only.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/collections/posts?where%5B_status%5D%5Bequals%5D=published")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert_eq!(
        count_table_rows(&body),
        1,
        "?where[_status][equals]=published should narrow to 1 published row"
    );
    assert!(body.contains("Live Article"));
    assert!(!body.contains("Pending Draft"));

    // Two top-level `_status` rows AND together like any other rows: no
    // document is both a draft and published, so the list is empty.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(
                "/admin/collections/posts?where%5B_status%5D%5Bequals%5D=draft\
                 &where%5B_status%5D%5Bequals%5D=published",
            )
            .header("cookie", &cookie)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert_eq!(
        count_table_rows(&body),
        0,
        "draft AND published must match no document"
    );
    assert!(!body.contains("Live Article"));
    assert!(!body.contains("Pending Draft"));

    // Empty `where[_status][equals]=` value (the "All" option in the
    // filter drawer) should fall through to showing both rows — the
    // extractor returns None for empty values, the filter UI's
    // `_collectFilters` skips empty-value rows, but both forms must
    // resolve to the unfiltered list.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/collections/posts?where%5B_status%5D%5Bequals%5D=")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_string(resp.into_body()).await;
    assert_eq!(
        count_table_rows(&body),
        2,
        "empty where[_status][equals]= (All) should show both draft and published"
    );

    // Multiple `_status` values across an OR-clause (`(_status=draft OR
    // _status=published)`) widen back to "show both" — the extractor
    // collects every value, the service injects `_status IN (...)`.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(
                "/admin/collections/posts?where[or][0][0][_status][equals]=draft\
                 &where[or][0][1][_status][equals]=published",
            )
            .header("cookie", &cookie)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert_eq!(
        count_table_rows(&body),
        2,
        "_status IN (draft, published) should show both rows"
    );
    assert!(body.contains("Live Article"));
    assert!(body.contains("Pending Draft"));
}

/// Regression: `?where[title][equals]=A&where[title][equals]=B` — two rows the
/// filter builder labels AND — were merged into `title IN ('A', 'B')` and
/// listed rows matching either. AND rows now AND (no single title is both);
/// "any of" is the `where[or][G][N][…]` URL form, which widens to a true OR.
#[tokio::test]
async fn list_items_or_clause_widens_results() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "or-filter@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "or-filter@test.com");

    let def = {
        let reg = &app.registry;
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    for title in ["Alpha", "Bravo", "Charlie"] {
        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!(title));
        query::create(&tx, "posts", &def, &data, None).unwrap();
    }
    tx.commit().unwrap();
    drop(conn);

    fn count_table_rows(body: &str) -> usize {
        let Some(start) = body.find("<tbody") else {
            return 0;
        };
        let Some(end) = body[start..].find("</tbody>").map(|i| start + i) else {
            return 0;
        };
        body[start..end].matches("<tr").count()
    }

    // Two AND-ed `equals` rows on `title`: no row's title is both.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(
                "/admin/collections/posts?where[title][equals]=Alpha&where[title][equals]=Bravo",
            )
            .header("cookie", &cookie)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert_eq!(
        count_table_rows(&body),
        0,
        "title = Alpha AND title = Bravo matches no row"
    );

    // Cross-field OR via `where[or][G][N][…]`: title=Alpha OR title=Charlie.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(
                "/admin/collections/posts?\
                 where[or][0][0][title][equals]=Alpha\
                 &where[or][0][1][title][equals]=Charlie",
            )
            .header("cookie", &cookie)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert_eq!(count_table_rows(&body), 2, "OR clause returns 2 rows");
    assert!(body.contains("Alpha"));
    assert!(body.contains("Charlie"));
    assert!(!body.contains("Bravo"));
}

/// Regression test for the user-reported "filter has no effect" symptom.
/// Both `where[status][equals]=draft` (raw) and the URL-encoded form
/// `where%5Bstatus%5D%5Bequals%5D=draft` (which is what the browser
/// produces when you click Apply) must narrow the list to draft items
/// only.
#[tokio::test]
async fn list_items_url_filter_narrows_results() {
    fn posts_with_status_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.timestamps = true;
        def.admin.use_as_title = Some("title".to_string());
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .required(true)
                .build(),
            FieldDefinition::builder("status", FieldType::Select)
                .options(vec![
                    SelectOption::new(LocalizedString::Plain("Draft".to_string()), "draft"),
                    SelectOption::new(LocalizedString::Plain("Published".to_string()), "published"),
                ])
                .build(),
        ];
        def
    }

    /// Count `<tr>` rows inside the items `<tbody>` of the rendered list page.
    fn count_table_rows(body: &str) -> usize {
        let Some(start) = body.find("<tbody") else {
            return 0;
        };
        let Some(end) = body[start..].find("</tbody>").map(|i| start + i) else {
            return 0;
        };
        body[start..end].matches("<tr").count()
    }

    let app = setup_app(vec![posts_with_status_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "filter@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "filter@test.com");

    // Insert one draft + one published post.
    let def = {
        let reg = &app.registry;
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let mut data1 = DocumentFields::new();
    data1.insert("title".to_string(), json!("Draft Post"));
    data1.insert("status".to_string(), json!("draft"));
    query::create(&tx, "posts", &def, &data1, None).unwrap();
    let mut data2 = DocumentFields::new();
    data2.insert("title".to_string(), json!("Published Post"));
    data2.insert("status".to_string(), json!("published"));
    query::create(&tx, "posts", &def, &data2, None).unwrap();
    tx.commit().unwrap();
    drop(conn);

    // Sanity: no filter shows both.
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
    let body = body_string(resp.into_body()).await;
    assert_eq!(
        count_table_rows(&body),
        2,
        "unfiltered list should show both posts"
    );
    assert!(
        body.contains("Draft Post"),
        "title 'Draft Post' must appear"
    );
    assert!(
        body.contains("Published Post"),
        "title 'Published Post' must appear"
    );

    // Filter via raw `where[status][equals]=draft`. List should only
    // show the draft post.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/collections/posts?where[status][equals]=draft")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert_eq!(
        count_table_rows(&body),
        1,
        "raw-filter list should narrow to 1 row (the draft post)"
    );
    assert!(body.contains("Draft Post"));
    assert!(
        !body.contains("Published Post"),
        "raw-filter list must NOT contain the published post"
    );

    // Filter via URL-encoded `where%5Bstatus%5D%5Bequals%5D=draft` — what
    // the browser actually sends when JS builds the URL.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/collections/posts?where%5Bstatus%5D%5Bequals%5D=draft")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert_eq!(
        count_table_rows(&body),
        1,
        "encoded-filter list should narrow to 1 row"
    );
    assert!(body.contains("Draft Post"));
    assert!(
        !body.contains("Published Post"),
        "encoded-filter list must NOT contain the published post"
    );
}

#[tokio::test]
async fn list_items_with_search() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "search@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "search@test.com");

    let def = {
        let reg = &app.registry;
        reg.get_collection("posts").unwrap().clone()
    };
    for title in &["Zebra Unique Alpha", "Beta Common", "Gamma Common"] {
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let data: DocumentFields = HashMap::from([("title".to_string(), json!(title))]).into();
        query::create(&tx, "posts", &def, &data, None).unwrap();
        tx.commit().unwrap();
    }

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts?search=Zebra")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(
        body.contains("Zebra"),
        "Search results should contain 'Zebra'"
    );
}

#[tokio::test]
async fn search_uses_configured_searchable_fields() {
    let app = setup_app(vec![make_searchable_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "search2@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "search2@test.com");

    let def = {
        let reg = &app.registry;
        reg.get_collection("sposts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields = HashMap::from([
        ("title".to_string(), json!("Unique Title XYZ")),
        ("body".to_string(), json!("Some body text")),
        ("category".to_string(), json!("tech")),
    ])
    .into();
    let doc = query::create(&tx, "sposts", &def, &data, None).unwrap();
    query::fts::fts_upsert(
        &tx,
        &query::fts::FtsIndex::builder("sposts", &def, &LocaleConfig::default()).build(),
        &doc.id,
    )
    .unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/sposts?search=Unique")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(
        body.contains("Unique Title XYZ"),
        "Search should find by configured searchable fields"
    );
}

#[tokio::test]
async fn list_items_search_with_special_chars() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "special@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "special@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts?search=hello%20world%26foo")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn list_items_search_no_results() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "nosearch@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "nosearch@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts?search=nonexistent_query_xyz")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Regression: a search with no hits showed the "no posts yet / create
    // the first one" empty state.
    let body = body_string(resp.into_body()).await;
    assert!(body.contains("No results"), "filtered empty state");
    assert!(
        !body.contains("folder_open"),
        "not the unfiltered empty state"
    );
}

#[tokio::test]
async fn list_items_with_search_and_pagination() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "sp@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "sp@test.com");

    let def = {
        let reg = &app.registry;
        reg.get_collection("posts").unwrap().clone()
    };
    for i in 0..5 {
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let data: DocumentFields =
            HashMap::from([("title".to_string(), json!(format!("Searchable Item {}", i)))]).into();
        let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
        query::fts::fts_upsert(
            &tx,
            &query::fts::FtsIndex::builder("posts", &def, &LocaleConfig::default()).build(),
            &doc.id,
        )
        .unwrap();
        tx.commit().unwrap();
    }

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts?search=Searchable&page=1&per_page=3")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    // 5 results, per_page=3 → 2 pages with pagination and Next link
    assert!(body.contains("Page 1 of 2"), "should show page info");
    assert!(body.contains("Next"), "should have Next link");
    // Regression: the page links dropped `per_page`, so page 2 came back at
    // the default size and overlapped page 1. The search form carries it too.
    let decoded = body.replace("&#x3D;", "=").replace("&amp;", "&");
    assert!(
        decoded.contains("page=2&per_page=3"),
        "the Next link keeps per_page"
    );
    assert!(
        decoded.contains("name=\"per_page\""),
        "the search form keeps per_page"
    );
    assert!(
        !body.contains("Previous"),
        "page 1 should not have Previous link"
    );
}

#[tokio::test]
async fn list_items_with_pagination_renders_docs() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "page@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "page@test.com");

    let def = {
        let reg = &app.registry;
        reg.get_collection("posts").unwrap().clone()
    };
    for i in 0..25 {
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let data: DocumentFields =
            HashMap::from([("title".to_string(), json!(format!("Post {}", i)))]).into();
        query::create(&tx, "posts", &def, &data, None).unwrap();
        tx.commit().unwrap();
    }

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/collections/posts?page=2&per_page=10")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(body.contains("Page 2 of 3"), "should show page 2 of 3");
    assert!(
        body.contains("Previous"),
        "middle page should have Previous"
    );
    assert!(body.contains("Next"), "middle page should have Next");
}

#[tokio::test]
async fn collection_list_pagination_multi_page_shows_nav() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "page@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "page@test.com");

    let def = {
        let reg = &app.registry;
        reg.get_collection("posts").unwrap().clone()
    };
    for i in 0..5 {
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let data: DocumentFields =
            HashMap::from([("title".to_string(), json!(format!("Post {}", i)))]).into();
        query::create(&tx, "posts", &def, &data, None).unwrap();
        tx.commit().unwrap();
    }

    // Page 1 of 3 → has "Next", no "Previous", shows "Page 1 of 3"
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/collections/posts?page=1&per_page=2")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(body.contains("Page 1 of 3"), "should show page info");
    assert!(body.contains("Next"), "page 1 should have Next link");
    assert!(
        !body.contains("Previous"),
        "page 1 should not have Previous link"
    );

    // Page 2 of 3 → has both "Previous" and "Next"
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/collections/posts?page=2&per_page=2")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(body.contains("Page 2 of 3"), "should show page info");
    assert!(body.contains("Next"), "page 2 should have Next link");
    assert!(
        body.contains("Previous"),
        "page 2 should have Previous link"
    );

    // Page 3 of 3 → has "Previous", no "Next"
    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts?page=3&per_page=2")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(body.contains("Page 3 of 3"), "should show page info");
    assert!(
        !body.contains("Next"),
        "last page should not have Next link"
    );
    assert!(
        body.contains("Previous"),
        "last page should have Previous link"
    );
}

#[tokio::test]
async fn collection_list_pagination_single_page_no_nav() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "single@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "single@test.com");

    let def = {
        let reg = &app.registry;
        reg.get_collection("posts").unwrap().clone()
    };
    for i in 0..3 {
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let data: DocumentFields =
            HashMap::from([("title".to_string(), json!(format!("Post {}", i)))]).into();
        query::create(&tx, "posts", &def, &data, None).unwrap();
        tx.commit().unwrap();
    }

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts?page=1&per_page=10")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    // 3 docs fit in 1 page of 10 → no navigation links
    assert!(
        !body.contains("Previous"),
        "single page should not have Previous"
    );
    assert!(!body.contains("Next"), "single page should not have Next");
}

/// Regression: `_status` is a column only on a collection that keeps drafts.
/// The sort gate accepted it everywhere, so the query named a column the
/// table never had — a 500 where an unknown sort key owes a 400.
#[tokio::test]
async fn sorting_by_status_needs_a_collection_with_drafts() {
    let app = setup_app(
        vec![
            make_posts_def(),
            make_versioned_posts_def(),
            make_users_def(),
        ],
        vec![],
    );
    let user_id = create_test_user(&app, "sorter@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "sorter@test.com");

    assert_eq!(
        status_sorted_list(&app, &cookie, "posts").await,
        StatusCode::BAD_REQUEST,
        "no drafts, no `_status` column to sort on"
    );
    assert_eq!(
        status_sorted_list(&app, &cookie, "articles").await,
        StatusCode::OK,
        "a drafts collection sorts by status"
    );
}
