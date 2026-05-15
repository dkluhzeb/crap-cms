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
use std::collections::HashMap;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::json;
use tower::ServiceExt;

use crap_cms::{
    core::{DocumentFields, collection::*, field::*},
    db::query,
};

use crap_cms_e2e::{helpers::*, html};

// ── Helpers ───────────────────────────────────────────────────────────────

fn make_all_field_types_def() -> CollectionDefinition {
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
        FieldDefinition::builder("count", FieldType::Number).build(),
        FieldDefinition::builder("contact", FieldType::Email).build(),
        FieldDefinition::builder("body", FieldType::Textarea).build(),
        FieldDefinition::builder("category", FieldType::Select)
            .options(vec![
                SelectOption::new(LocalizedString::Plain("News".to_string()), "news"),
                SelectOption::new(LocalizedString::Plain("Blog".to_string()), "blog"),
            ])
            .build(),
        FieldDefinition::builder("featured", FieldType::Checkbox).build(),
        FieldDefinition::builder("published_at", FieldType::Date).build(),
    ];
    def
}

fn make_array_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("teams");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Team".to_string())),
        plural: Some(LocalizedString::Plain("Teams".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("name", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("members", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("member_name", FieldType::Text)
                    .required(true)
                    .build(),
                FieldDefinition::builder("role", FieldType::Text).build(),
            ])
            .build(),
    ];
    def.admin = AdminConfig {
        use_as_title: Some("name".to_string()),
        ..AdminConfig::default()
    };
    def
}

fn make_blocks_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("pages");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Page".to_string())),
        plural: Some(LocalizedString::Plain("Pages".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![
                BlockDefinition {
                    block_type: "paragraph".to_string(),
                    fields: vec![FieldDefinition::builder("text", FieldType::Textarea).build()],
                    label: Some(LocalizedString::Plain("Paragraph".to_string())),
                    ..Default::default()
                },
                BlockDefinition {
                    block_type: "heading".to_string(),
                    fields: vec![
                        FieldDefinition::builder("text", FieldType::Text).build(),
                        FieldDefinition::builder("level", FieldType::Select)
                            .options(vec![
                                SelectOption::new(LocalizedString::Plain("H2".to_string()), "h2"),
                                SelectOption::new(LocalizedString::Plain("H3".to_string()), "h3"),
                            ])
                            .build(),
                    ],
                    label: Some(LocalizedString::Plain("Heading".to_string())),
                    ..Default::default()
                },
            ])
            .build(),
    ];
    def
}

fn make_tabs_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("profiles");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Profile".to_string())),
        plural: Some(LocalizedString::Plain("Profiles".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("info", FieldType::Tabs)
            .tabs(vec![
                FieldTab {
                    label: "Basic".to_string(),
                    description: None,
                    fields: vec![
                        FieldDefinition::builder("first_name", FieldType::Text)
                            .required(true)
                            .build(),
                        FieldDefinition::builder("last_name", FieldType::Text).build(),
                    ],
                },
                FieldTab {
                    label: "Contact".to_string(),
                    description: None,
                    fields: vec![
                        FieldDefinition::builder("email", FieldType::Email).build(),
                        FieldDefinition::builder("phone", FieldType::Text).build(),
                    ],
                },
            ])
            .build(),
    ];
    def
}

fn make_group_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("events");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Event".to_string())),
        plural: Some(LocalizedString::Plain("Events".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("location", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("city", FieldType::Text).build(),
                FieldDefinition::builder("venue", FieldType::Text).build(),
            ])
            .build(),
    ];
    def
}

fn make_row_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("contacts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Contact".to_string())),
        plural: Some(LocalizedString::Plain("Contacts".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("name_row", FieldType::Row)
            .fields(vec![
                FieldDefinition::builder("first_name", FieldType::Text)
                    .required(true)
                    .build(),
                FieldDefinition::builder("last_name", FieldType::Text).build(),
            ])
            .build(),
    ];
    def
}

fn make_nested_array_tabs_row_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("organizations");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Organization".to_string())),
        plural: Some(LocalizedString::Plain("Organizations".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("org_name", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("team_members", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("member_info", FieldType::Tabs)
                    .tabs(vec![
                        FieldTab {
                            label: "Name".to_string(),
                            description: None,
                            fields: vec![
                                FieldDefinition::builder("names", FieldType::Row)
                                    .fields(vec![
                                        FieldDefinition::builder("first_name", FieldType::Text)
                                            .required(true)
                                            .build(),
                                        FieldDefinition::builder("last_name", FieldType::Text)
                                            .build(),
                                    ])
                                    .build(),
                            ],
                        },
                        FieldTab {
                            label: "Details".to_string(),
                            description: None,
                            fields: vec![FieldDefinition::builder("role", FieldType::Text).build()],
                        },
                    ])
                    .build(),
            ])
            .build(),
    ];
    def
}

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

// ── 1. create_form_renders_all_field_types ────────────────────────────────

#[tokio::test]
async fn create_form_renders_all_field_types() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_all_field_types_def(), make_users_def()],
        vec![],
        "fields@test.com",
        "pass123",
    );

    let body = get_create_form(&app, "articles", &cookie).await;
    let doc = html::parse(&body);

    html::assert_field_exists(&doc, "title");
    html::assert_field_exists(&doc, "count");
    html::assert_field_exists(&doc, "contact");
    html::assert_field_exists(&doc, "body");
    html::assert_field_exists(&doc, "category");
    html::assert_field_exists(&doc, "featured");
    html::assert_field_exists(&doc, "published_at");

    // Check input types
    html::assert_exists(&doc, "input[name=\"title\"][type=\"text\"]", "title input");
    html::assert_exists(
        &doc,
        "input[name=\"count\"][type=\"number\"]",
        "count input",
    );
    html::assert_exists(
        &doc,
        "input[name=\"contact\"][type=\"email\"]",
        "contact input",
    );
    html::assert_exists(&doc, "textarea[name=\"body\"]", "body textarea");
    html::assert_exists(&doc, "select[name=\"category\"]", "category select");
    html::assert_exists(
        &doc,
        "input[name=\"featured\"][type=\"checkbox\"]",
        "featured checkbox",
    );

    // Required field should have required attr
    html::assert_exists(
        &doc,
        "input[name=\"title\"][required]",
        "title should be required",
    );
}

// ── 2. edit_form_populates_values ─────────────────────────────────────────

#[tokio::test]
async fn edit_form_populates_values() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_all_field_types_def(), make_users_def()],
        vec![],
        "edit@test.com",
        "pass123",
    );

    let def = app.registry.get_collection("articles").unwrap().clone();
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields = HashMap::from([
        ("title".to_string(), json!("My Article")),
        ("count".to_string(), json!("42")),
        ("contact".to_string(), json!("test@example.com")),
        ("body".to_string(), json!("Article body text")),
    ])
    .into();
    let doc_record = query::create(&tx, "articles", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!("/admin/collections/articles/{}", doc_record.id))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let doc = html::parse(&body);

    html::assert_input(&doc, "title", "text", Some("My Article"));
    // Number fields store as float internally
    let count_el = html::select_one(&doc, "input[name=\"count\"]");
    let count_val = count_el.value().attr("value").unwrap_or("");
    assert!(
        count_val == "42" || count_val == "42.0",
        "count value should be 42 or 42.0, got {count_val:?}"
    );
    html::assert_input(&doc, "contact", "email", Some("test@example.com"));

    // Textarea value is inner text, not value attr
    let textarea = html::select_one(&doc, "textarea[name=\"body\"]");
    let text: String = textarea.text().collect();
    assert!(
        text.contains("Article body text"),
        "textarea should contain body text, got: {text:?}"
    );
}

// ── 3. create_form_array_field_structure ──────────────────────────────────

#[tokio::test]
async fn create_form_array_field_structure() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_array_def(), make_users_def()],
        vec![],
        "array@test.com",
        "pass123",
    );

    let body = get_create_form(&app, "teams", &cookie).await;
    let doc = html::parse(&body);

    html::assert_field_exists(&doc, "members");
    html::assert_exists(
        &doc,
        "[data-field-type=\"array\"]",
        "array field type marker",
    );

    // Template for new rows should exist
    html::assert_exists(&doc, "template", "array template for new rows");

    // Add row button
    html::assert_exists(
        &doc,
        "button[data-action=\"add-array-row\"]",
        "add row button",
    );
}

// ── 4. edit_form_array_populated_rows ─────────────────────────────────────

#[tokio::test]
async fn edit_form_array_populated_rows() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_array_def(), make_users_def()],
        vec![],
        "arraypop@test.com",
        "pass123",
    );

    // Create via HTTP POST to get array data stored properly
    let create_resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/collections/teams")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(
                    "name=Alpha+Team&members[0][member_name]=Alice&members[0][role]=Lead&members[1][member_name]=Bob&members[1][role]=Dev",
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = create_resp.status();
    // 200 with HX-Redirect or 303 are both valid success responses
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND || status == StatusCode::OK,
        "create should succeed, got {status}"
    );

    // Find the created document by listing the collection
    let list_resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/collections/teams")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list_resp.status(), StatusCode::OK);
    let list_body = body_string(list_resp.into_body()).await;
    assert!(
        list_body.contains("Alpha Team"),
        "list should contain the created team"
    );

    // Extract an edit link from the list page
    let list_doc = html::parse(&list_body);
    let edit_link = html::select_one(&list_doc, "table tbody tr a[href]");
    let href = edit_link.value().attr("href").unwrap();

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

    let rows = html::count(&doc, ".form__array-row");
    assert!(rows >= 2, "should have at least 2 array rows, got {rows}");

    // Sub-field inputs should use array naming
    html::assert_exists(
        &doc,
        "input[name=\"members[0][member_name]\"]",
        "first row member_name",
    );
}

// ── 5. create_form_blocks_field_structure ─────────────────────────────────

#[tokio::test]
async fn create_form_blocks_field_structure() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_blocks_def(), make_users_def()],
        vec![],
        "blocks@test.com",
        "pass123",
    );

    let body = get_create_form(&app, "pages", &cookie).await;
    let doc = html::parse(&body);

    html::assert_field_exists(&doc, "content");
    html::assert_exists(
        &doc,
        "[data-field-type=\"blocks\"]",
        "blocks field type marker",
    );

    // One template per block type
    let templates = html::count(&doc, "template");
    assert!(
        templates >= 2,
        "should have at least 2 block templates (paragraph + heading), got {templates}"
    );

    // Block picker / add button
    html::assert_exists(
        &doc,
        "button[data-action=\"add-block-row\"]",
        "add block button",
    );
}

// ── 6. create_form_tabs_layout ────────────────────────────────────────────

#[tokio::test]
async fn create_form_tabs_layout() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_tabs_def(), make_users_def()],
        vec![],
        "tabs@test.com",
        "pass123",
    );

    let body = get_create_form(&app, "profiles", &cookie).await;
    let doc = html::parse(&body);

    // Tab buttons
    let tab_buttons = html::select_all(&doc, "[role=\"tab\"]");
    assert_eq!(
        tab_buttons.len(),
        2,
        "should have 2 tab buttons, got {}",
        tab_buttons.len()
    );

    // Tab panels
    let tab_panels = html::select_all(&doc, "[role=\"tabpanel\"]");
    assert_eq!(
        tab_panels.len(),
        2,
        "should have 2 tab panels, got {}",
        tab_panels.len()
    );

    // Fields inside panels
    html::assert_field_exists(&doc, "first_name");
    html::assert_field_exists(&doc, "last_name");
    html::assert_field_exists(&doc, "email");
    html::assert_field_exists(&doc, "phone");
}

// ── 7. create_form_group_layout ───────────────────────────────────────────

#[tokio::test]
async fn create_form_group_layout() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_group_def(), make_users_def()],
        vec![],
        "group@test.com",
        "pass123",
    );

    let body = get_create_form(&app, "events", &cookie).await;
    let doc = html::parse(&body);

    // Group fieldset
    html::assert_exists(&doc, "fieldset.form__group", "group fieldset");

    // Sub-fields within group use prefixed names (group__subfield)
    html::assert_field_exists(&doc, "location__city");
    html::assert_field_exists(&doc, "location__venue");
}

// ── 8. create_form_row_layout ─────────────────────────────────────────────

#[tokio::test]
async fn create_form_row_layout() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_row_def(), make_users_def()],
        vec![],
        "row@test.com",
        "pass123",
    );

    let body = get_create_form(&app, "contacts", &cookie).await;
    let doc = html::parse(&body);

    // Row wrapper
    html::assert_exists(&doc, ".form__row", "row wrapper");

    // Sub-fields within row
    html::assert_field_exists(&doc, "first_name");
    html::assert_field_exists(&doc, "last_name");
}

// ── 9. create_form_nested_array_tabs_row ──────────────────────────────────

#[tokio::test]
async fn create_form_nested_array_tabs_row() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_nested_array_tabs_row_def(), make_users_def()],
        vec![],
        "nested@test.com",
        "pass123",
    );

    let body = get_create_form(&app, "organizations", &cookie).await;
    let doc = html::parse(&body);

    // Top-level array
    html::assert_field_exists(&doc, "team_members");
    html::assert_exists(
        &doc,
        "[data-field-type=\"array\"]",
        "array field type marker",
    );

    // The template should contain nested structure; check it's present
    html::assert_exists(&doc, "template", "array template");

    // Template content uses __INDEX__ placeholder in data-field-name.
    // Verify the template HTML has nested field names with bracketed naming.
    assert!(
        body.contains("data-field-name=")
            && (body.contains("first_name") || body.contains("member_info")),
        "nested template should contain sub-field data-field-name attributes"
    );
}

// ── 10. create_form_auth_collection ───────────────────────────────────────

#[tokio::test]
async fn create_form_auth_collection() {
    let HtmlTestCtx { app, cookie, .. } =
        setup_html_test(vec![make_users_def()], vec![], "auth@test.com", "pass123");

    let body = get_create_form(&app, "users", &cookie).await;
    let doc = html::parse(&body);

    // Auth collection create form should have a password field
    html::assert_exists(
        &doc,
        "input[name=\"password\"]",
        "password field on auth collection create",
    );
}
