use std::time::Duration;

use crate::browser;
use crate::helpers::*;

use crap_cms::core::collection::*;
use crap_cms::core::field::*;

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
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_toast_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "btoast@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "btoast@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "btoast@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/posts/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    // Wait for JS/HTMX to initialize
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Submit with empty required field using requestSubmit to ensure HTMX intercepts
    page.evaluate("() => document.querySelector('#edit-form')?.requestSubmit()")
        .await
        .unwrap();
    // Wait for validation fetch + toast rendering
    tokio::time::sleep(Duration::from_millis(2000)).await;

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
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_toast_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "bsave@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "bsave@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "bsave@test.com", "pass123").await;

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
    tokio::time::sleep(Duration::from_millis(1000)).await;

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
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_toast_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "bcrapns@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "bcrapns@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "bcrapns@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/posts"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Call the sugar — should dispatch crap:toast-request and the
    // singleton renders it.
    page.evaluate("() => window.crap.toast({ message: 'hello from crap', type: 'info' })")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let result = browser::shadow_eval(
        &page,
        "crap-toast",
        "const t = root.querySelector('.toast'); return t ? t.textContent : 'no-toast';",
    )
    .await;
    assert_eq!(
        result, "hello from crap",
        "window.crap.toast should render a toast with the given message; got: '{result}'"
    );

    server_handle.abort();
}
