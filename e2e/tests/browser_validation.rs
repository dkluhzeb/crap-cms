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
use std::time::Duration;

use tokio::time::sleep;

use crap_cms::core::{collection::*, field::*};

use crap_cms_e2e::{BrowserTestCtx, browser, helpers::*, setup_browser_test};

fn make_validated_def() -> CollectionDefinition {
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
    def
}

// ── 25. client_side_validation_shows_errors ───────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn client_side_validation_shows_errors() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        ..
    } = setup_browser_test(
        vec![make_validated_def(), make_users_def()],
        vec![],
        "bval@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/posts/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    // Wait for JS/HTMX to initialize
    sleep(Duration::from_millis(500)).await;

    // Submit with empty required field using requestSubmit
    page.evaluate("() => document.querySelector('#edit-form')?.requestSubmit()")
        .await
        .unwrap();
    // Wait for validation fetch + error rendering
    sleep(Duration::from_secs(2)).await;

    let result = page
        .evaluate("() => document.querySelectorAll('.form__error').length")
        .await
        .unwrap();
    let error_count: i64 = result.into_value().unwrap_or(0);
    assert!(
        error_count > 0,
        "should show .form__error after submitting empty required field"
    );

    server_handle.abort();
}

// ── 26. validation_clears_on_valid_resubmit ───────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn validation_clears_on_valid_resubmit() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        ..
    } = setup_browser_test(
        vec![make_validated_def(), make_users_def()],
        vec![],
        "bclear@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/posts/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    // Wait for JS/HTMX to initialize
    sleep(Duration::from_millis(500)).await;

    // Trigger validation error
    page.evaluate("() => document.querySelector('#edit-form')?.requestSubmit()")
        .await
        .unwrap();
    sleep(Duration::from_secs(2)).await;

    // Fill in the required field
    page.find_element("input[name=\"title\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap()
        .type_str("Valid Title")
        .await
        .unwrap();

    // Resubmit via requestSubmit
    page.evaluate("() => document.querySelector('#edit-form')?.requestSubmit()")
        .await
        .unwrap();
    sleep(Duration::from_secs(2)).await;

    let result = page
        .evaluate("() => document.querySelectorAll('.form__error').length")
        .await
        .unwrap();
    let error_count: i64 = result.into_value().unwrap_or(0);
    assert_eq!(
        error_count, 0,
        "errors should be cleared after valid resubmit, got {error_count}"
    );

    server_handle.abort();
}

// ── 27. validation_expands_collapsed_array_row ────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn validation_expands_collapsed_array_row() {
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

    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        ..
    } = setup_browser_test(
        vec![def, make_users_def()],
        vec![],
        "barray@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/teams/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    // Wait for JS to initialize
    sleep(Duration::from_millis(500)).await;

    // Add a row
    page.evaluate("() => document.querySelector('button[data-action=\"add-array-row\"]')?.click()")
        .await
        .unwrap();
    sleep(Duration::from_millis(300)).await;

    // Fill name but leave array sub-field empty
    page.find_element("input[name=\"name\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap()
        .type_str("Test Team")
        .await
        .unwrap();

    // Submit via requestSubmit to trigger HTMX validation
    page.evaluate("() => document.querySelector('#edit-form')?.requestSubmit()")
        .await
        .unwrap();
    sleep(Duration::from_secs(2)).await;

    // Error badge or expanded state should appear on the array row
    let result = page
        .evaluate(
            "() => document.querySelectorAll('.form__array-row--has-errors, .form__array-row-error-badge, .form__error').length",
        )
        .await
        .unwrap();
    let badge_count: i64 = result.into_value().unwrap_or(0);
    assert!(
        badge_count > 0,
        "array row with validation error should show error indicator"
    );

    server_handle.abort();
}

// ── save_as_draft_skips_required_via_validate_endpoint ────────────────────
//
// Regression: clicking "Save as draft" with empty required fields used
// to fail because `<crap-validate-form>::_collectFormData` built its
// FormData via `new FormData(form).entries()`, which silently drops
// the *submitter* button's `name=value`. The result: `_action` was
// missing from the validate request → server's `validate_create`
// computed `is_draft = false` → required-field check fired →
// inline `Field is required` errors shown on a draft save the user
// explicitly asked for. Fix: track the last clicked submit button
// on the component (`_lastSubmitter`) and pass it as the second arg
// to `new FormData(form, submitter)` (or fall back to manual
// append on browsers without that constructor signature).

fn make_versioned_required_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def.timestamps = true;
    def.versions = Some(VersionsConfig::new(true, 10));
    def.admin = AdminConfig {
        use_as_title: Some("title".to_string()),
        ..AdminConfig::default()
    };
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("body", FieldType::Textarea).build(),
    ];
    def
}

#[tokio::test(flavor = "multi_thread")]
async fn save_as_draft_skips_required_via_validate_endpoint() {
    let (base_url, server_handle, app) = browser::spawn_server(
        vec![make_versioned_required_def(), make_users_def()],
        vec![],
    )
    .await;
    let user_id = create_test_user(&app, "vdraft@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "vdraft@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "vdraft@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/posts/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    sleep(Duration::from_millis(500)).await;

    // Leave `title` (required) empty. Click "Save as draft" — the
    // button with `name="_action" value="save_draft"`.
    page.evaluate("() => document.querySelector('button[value=\"save_draft\"]')?.click()")
        .await
        .unwrap();
    sleep(Duration::from_secs(2)).await;

    // No inline `form__error` with `data-validate-error` should be
    // present. Pre-fix, the validate endpoint would return
    // `{title: "Field is required"}` and the JS would render an
    // inline error message next to the title input.
    let inline_errors = page
        .evaluate(
            "() => Array.from(document.querySelectorAll('.form__error[data-validate-error]')).map(e => e.textContent).join(' | ')",
        )
        .await
        .unwrap()
        .into_value::<String>()
        .unwrap_or_default();
    assert!(
        inline_errors.is_empty(),
        "save_draft must not produce inline required-field errors (got: {inline_errors:?})"
    );

    server_handle.abort();
}
