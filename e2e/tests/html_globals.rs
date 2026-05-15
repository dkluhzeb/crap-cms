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

fn make_global_with_fields() -> crap_cms::core::collection::GlobalDefinition {
    let mut def = GlobalDefinition::new("settings");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Settings".to_string())),
        plural: None,
    };
    def.fields = vec![
        FieldDefinition::builder("site_name", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("tagline", FieldType::Text).build(),
    ];
    def
}

// ── 23. global_edit_form_renders_fields ───────────────────────────────────

#[tokio::test]
async fn global_edit_form_renders_fields() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_users_def()],
        vec![make_global_with_fields()],
        "global@test.com",
        "pass123",
    );

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/globals/settings")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let doc = html::parse(&body);

    html::assert_field_exists(&doc, "site_name");
    html::assert_field_exists(&doc, "tagline");
    html::assert_exists(&doc, "input[name=\"site_name\"]", "site_name input");
    html::assert_exists(&doc, "input[name=\"tagline\"]", "tagline input");
}

// ── 24. global_form_has_validate_wrapper ──────────────────────────────────

#[tokio::test]
async fn global_form_has_validate_wrapper() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_users_def()],
        vec![make_global_with_fields()],
        "gval@test.com",
        "pass123",
    );

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/globals/settings")
                .header("cookie", &cookie)
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
        "crap-validate-form",
        "global form should be wrapped in <crap-validate-form>",
    );
}

// ── global_edit_form_has_loading_indicator ────────────────────────────────
//
// Regression: globals/edit.hbs was missing `hx-indicator="#upload-loading"`
// AND globals/edit_sidebar.hbs was missing the corresponding indicator
// markup, so the user got zero visual feedback during a global save.

#[tokio::test]
async fn global_edit_form_has_loading_indicator() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_users_def()],
        vec![make_global_with_fields()],
        "gload@test.com",
        "pass123",
    );

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/globals/settings")
                .header("cookie", &cookie)
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
        "form#edit-form[hx-indicator=\"#upload-loading\"]",
        "global edit form must declare hx-indicator so the spinner fires",
    );
    html::assert_exists(
        &doc,
        "#upload-loading.edit-sidebar__save-indicator",
        "global edit sidebar must render the saving spinner element",
    );
}
