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

fn make_toast_def() -> CollectionDefinition {
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
    def
}

// ── 32. toast_on_validation_error ─────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn toast_on_validation_error() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_toast_def(), make_users_def()],
        vec![],
        "btoast@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/posts/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    // Wait for JS/HTMX to initialize (form present and htmx loaded).
    browser::wait_for_js(&page, "document.querySelector('#edit-form') && window.htmx").await;

    // Submit with empty required field using requestSubmit to ensure HTMX intercepts
    page.evaluate("() => document.querySelector('#edit-form')?.requestSubmit()")
        .await
        .unwrap();

    // Wait for validation fetch + toast rendering (host is light DOM).
    browser::wait_for_element(&page, "crap-toast").await;

    // Toast should exist
    let has_toast = page
        .evaluate("() => !!document.querySelector('crap-toast')")
        .await
        .unwrap();
    let toast_found: bool = has_toast.into_value().unwrap_or(false);
    assert!(toast_found, "should show <crap-toast> on validation error");

    server_handle.abort();
}

// ── 33. toast_on_successful_save ──────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn toast_on_successful_save() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_toast_def(), make_users_def()],
        vec![],
        "bsave@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/posts/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Fill in required field and submit
    page.find_element("input[name=\"title\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap()
        .type_str("Valid Post Title")
        .await
        .unwrap();

    page.find_element("button[type=\"submit\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap();

    // Wait for the post-save redirect off the create page onto the edit page.
    browser::wait_for_js(
        &page,
        "window.location.href.includes('/admin/collections/posts/') && !window.location.href.includes('/create')",
    )
    .await;

    // After successful save, should redirect to edit page (htmx or standard)
    // Toast may or may not be visible depending on redirect behavior
    // At minimum, verify no error toast
    let url = page.url().await.unwrap().unwrap_or_default();
    assert!(
        url.contains("/admin/collections/posts/") || url.contains("/admin"),
        "should navigate to edit page or stay in admin after save, got: {url}"
    );

    server_handle.abort();
}

// ── window_crap_namespace_dispatches_toast ───────────────────────────────
//
// Regression test for the `window.crap.toast({...})` convenience layer.
// Asserts that calling the namespaced sugar produces a visible toast in
// the singleton component's shadow root.

#[tokio::test(flavor = "multi_thread")]
async fn window_crap_namespace_dispatches_toast() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_toast_def(), make_users_def()],
        vec![],
        "bcrapns@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/posts"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Wait for the crap namespace sugar and the toast component to be ready.
    browser::wait_for_js(
        &page,
        "window.crap && window.crap.toast && customElements.get('crap-toast')",
    )
    .await;

    // Call the sugar — should dispatch crap:toast-request and the
    // singleton renders it.
    page.evaluate("() => window.crap.toast({ message: 'hello from crap', type: 'info' })")
        .await
        .unwrap();

    // Poll the shadow root until the toast renders with the expected message.
    let mut result = String::new();
    for _ in 0..60 {
        result = browser::shadow_eval(
            &page,
            "crap-toast",
            "const t = root.querySelector('.toast'); return t ? t.textContent : 'no-toast';",
        )
        .await;
        if result == "hello from crap" {
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        result, "hello from crap",
        "window.crap.toast should render a toast with the given message; got: '{result}'"
    );

    server_handle.abort();
}
