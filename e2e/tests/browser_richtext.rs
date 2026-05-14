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

use crap_cms_e2e::browser;
use crap_cms_e2e::helpers::*;

use crap_cms::core::collection::*;
use crap_cms::core::field::*;

fn make_richtext_def() -> CollectionDefinition {
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
        FieldDefinition::builder("body", FieldType::Richtext).build(),
    ];
    def
}

// ── richtext_renders_editor ──────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn richtext_renders_editor() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_richtext_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "brt1@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "brt1@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "brt1@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/articles/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Check for ProseMirror editor inside shadow root
    let has_editor = browser::shadow_eval(
        &page,
        "crap-richtext",
        "return root.querySelector('.ProseMirror') ? 'true' : 'false';",
    )
    .await;
    assert_eq!(
        has_editor, "true",
        "crap-richtext shadow root should contain .ProseMirror element"
    );

    server_handle.abort();
}

// ── richtext_typing_updates_hidden_input ──────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn richtext_typing_updates_hidden_input() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_richtext_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "brt2@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "brt2@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "brt2@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/articles/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Insert text via ProseMirror API
    page.evaluate(
        "() => { \
            const host = document.querySelector('crap-richtext'); \
            const view = host._view; \
            if (view) { \
                const tr = view.state.tr.insertText('Hello from ProseMirror'); \
                view.dispatch(tr); \
            } \
        }",
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Check that the hidden textarea reflects the update
    let result = page
        .evaluate("() => document.querySelector('crap-richtext textarea')?.value ?? ''")
        .await
        .unwrap();
    let textarea_val: String = result.into_value().unwrap();
    assert!(
        textarea_val.contains("Hello from ProseMirror"),
        "hidden textarea should contain typed text, got: {textarea_val}"
    );

    server_handle.abort();
}

// ── richtext_bold_toolbar ────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn richtext_bold_toolbar() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_richtext_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "brt3@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "brt3@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "brt3@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/articles/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Insert text, select all, and apply bold
    page.evaluate(
        "() => { \
            const host = document.querySelector('crap-richtext'); \
            const view = host._view; \
            if (view) { \
                let tr = view.state.tr.insertText('bold text'); \
                view.dispatch(tr); \
                tr = view.state.tr.setSelection( \
                    window.ProseMirror.TextSelection.create(view.state.doc, 1, view.state.doc.content.size - 1) \
                ); \
                view.dispatch(tr); \
            } \
        }",
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Click the bold button in the shadow root toolbar
    page.evaluate(
        "() => document.querySelector('crap-richtext').shadowRoot.querySelector('[data-cmd=\"bold\"]').click()",
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // The hidden textarea should contain <strong> tag
    let result = page
        .evaluate("() => document.querySelector('crap-richtext textarea')?.value ?? ''")
        .await
        .unwrap();
    let textarea_val: String = result.into_value().unwrap();
    assert!(
        textarea_val.contains("<strong>"),
        "textarea should contain <strong> after applying bold, got: {textarea_val}"
    );

    server_handle.abort();
}
