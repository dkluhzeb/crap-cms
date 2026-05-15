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
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

use crap_cms::core::{collection::*, field::*};

use crap_cms_e2e::{helpers::*, html};

// ── Helpers ───────────────────────────────────────────────────────────────

fn make_required_fields_def() -> CollectionDefinition {
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

fn make_array_required_sub_def() -> CollectionDefinition {
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
            ])
            .build(),
    ];
    def
}

fn make_nested_tabs_row_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("orgs");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Org".to_string())),
        plural: Some(LocalizedString::Plain("Orgs".to_string())),
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

fn make_array_group_required_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("projects");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Project".to_string())),
        plural: Some(LocalizedString::Plain("Projects".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("name", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("members", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("info", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("full_name", FieldType::Text)
                            .required(true)
                            .build(),
                        FieldDefinition::builder("role", FieldType::Text).build(),
                    ])
                    .build(),
            ])
            .build(),
    ];
    def
}

fn make_group_required_def() -> CollectionDefinition {
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
                FieldDefinition::builder("city", FieldType::Text)
                    .required(true)
                    .build(),
                FieldDefinition::builder("venue", FieldType::Text).build(),
            ])
            .build(),
    ];
    def
}

fn make_multi_required_def() -> CollectionDefinition {
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
        FieldDefinition::builder("slug", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("body", FieldType::Textarea).build(),
    ];
    def
}

async fn post_create(app: &TestApp, slug: &str, cookie: &str, form_body: &str) -> String {
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
    // Validation errors re-render the form (200), not redirect
    assert_eq!(
        status,
        StatusCode::OK,
        "validation error should re-render form (200), got {status}"
    );
    body_string(resp.into_body()).await
}

// ── 11. validation_error_on_required_field ────────────────────────────────

#[tokio::test]
async fn validation_error_on_required_field() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_required_fields_def(), make_users_def()],
        vec![],
        "val@test.com",
        "pass123",
    );

    let body = post_create(&app, "articles", &cookie, "title=&body=some+content").await;
    let doc = html::parse(&body);

    html::assert_field_exists(&doc, "title");
    html::assert_exists(
        &doc,
        "[data-field-name=\"title\"] .form__error",
        "title field should have error",
    );
}

// ── 12. validation_error_on_array_sub_field ───────────────────────────────

#[tokio::test]
async fn validation_error_on_array_sub_field() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_array_required_sub_def(), make_users_def()],
        vec![],
        "arrval@test.com",
        "pass123",
    );

    let body = post_create(
        &app,
        "teams",
        &cookie,
        "name=Alpha&members[0][member_name]=",
    )
    .await;
    let doc = html::parse(&body);

    // The array row should contain an error on the sub-field
    html::assert_exists(
        &doc,
        ".form__array-row .form__error",
        "array sub-field should have validation error",
    );
}

// ── 13. validation_error_on_nested_tabs_row_field ─────────────────────────

#[tokio::test]
async fn validation_error_on_nested_tabs_row_field() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_nested_tabs_row_def(), make_users_def()],
        vec![],
        "nested@test.com",
        "pass123",
    );

    let body = post_create(
        &app,
        "orgs",
        &cookie,
        "org_name=Acme&team_members[0][first_name]=&team_members[0][last_name]=Smith&team_members[0][role]=Dev",
    )
    .await;
    let doc = html::parse(&body);

    // The nested first_name inside Array > Tabs > Row should show an error
    html::assert_exists(
        &doc,
        ".form__array-row .form__error",
        "nested required field should have validation error",
    );
}

// ── 14. validation_error_on_group_sub_field ───────────────────────────────

#[tokio::test]
async fn validation_error_on_group_sub_field() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_group_required_def(), make_users_def()],
        vec![],
        "grpval@test.com",
        "pass123",
    );

    let body = post_create(
        &app,
        "events",
        &cookie,
        "title=Conference&location__city=&location__venue=Hall+A",
    )
    .await;
    let doc = html::parse(&body);

    // Group sub-field should have error (check within group or at field level)
    html::assert_exists(
        &doc,
        ".form__group .form__error, [data-field-name=\"location__city\"] .form__error",
        "group sub-field city should have validation error",
    );
}

// ── 15. multiple_validation_errors ────────────────────────────────────────

#[tokio::test]
async fn multiple_validation_errors() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_multi_required_def(), make_users_def()],
        vec![],
        "multi@test.com",
        "pass123",
    );

    let body = post_create(&app, "posts", &cookie, "title=&slug=&body=content").await;
    let doc = html::parse(&body);

    let error_count = html::count(&doc, ".form__error");
    assert!(
        error_count >= 2,
        "should have at least 2 validation errors, got {error_count}"
    );
}

// ── 16. validation_preserves_values_on_error ──────────────────────────────

#[tokio::test]
async fn validation_preserves_values_on_error() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_multi_required_def(), make_users_def()],
        vec![],
        "preserve@test.com",
        "pass123",
    );

    // Submit with title empty (required) but slug filled
    let body = post_create(&app, "posts", &cookie, "title=&slug=my-slug&body=content").await;
    let doc = html::parse(&body);

    // Title should have error
    html::assert_exists(
        &doc,
        "[data-field-name=\"title\"] .form__error",
        "title should have error",
    );

    // Slug value should be preserved in the re-rendered form
    html::assert_input(&doc, "slug", "text", Some("my-slug"));
}

// ── 17. validation_error_on_array_group_sub_field ────────────────────────

#[tokio::test]
async fn validation_error_on_array_group_sub_field() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_array_group_required_def(), make_users_def()],
        vec![],
        "agrp@test.com",
        "pass123",
    );

    // Submit with required full_name empty inside Array > Group
    let body = post_create(
        &app,
        "projects",
        &cookie,
        "name=MyProject&members[0][info][0][full_name]=&members[0][info][0][role]=Dev",
    )
    .await;
    let doc = html::parse(&body);

    // The array row should contain an error on the group sub-field
    html::assert_exists(
        &doc,
        ".form__array-row .form__error",
        "array > group sub-field should have validation error",
    );
}
