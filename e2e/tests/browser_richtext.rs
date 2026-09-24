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

use crap_cms::{
    config::CrapConfig,
    core::{collection::*, field::*},
};

use crap_cms_e2e::{
    BrowserTestCtx, browser, helpers::*, setup_browser_test, setup_browser_test_with_config,
};

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
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_richtext_def(), make_users_def()],
        vec![],
        "brt1@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/articles/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Poll the shadow root until the ProseMirror editor is mounted (shadow DOM,
    // so wait_for_js can't see it).
    let mut has_editor = String::new();
    for _ in 0..60 {
        has_editor = browser::shadow_eval(
            &page,
            "crap-richtext",
            "return root.querySelector('.ProseMirror') ? 'true' : 'false';",
        )
        .await;
        if has_editor == "true" {
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        has_editor, "true",
        "crap-richtext shadow root should contain .ProseMirror element"
    );

    server_handle.abort();
}

// ── richtext_typing_updates_hidden_input ──────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn richtext_typing_updates_hidden_input() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_richtext_def(), make_users_def()],
        vec![],
        "brt2@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/articles/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Wait for the editor to initialize (its `_view` becomes available).
    browser::wait_for_js(&page, "document.querySelector('crap-richtext')?._view").await;

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

    // Wait for the hidden textarea (light DOM) to reflect the typed text.
    browser::wait_for_js(
        &page,
        "(document.querySelector('crap-richtext textarea')?.value ?? '').includes('Hello from ProseMirror')",
    )
    .await;

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
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_richtext_def(), make_users_def()],
        vec![],
        "brt3@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/articles/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Wait for the editor to initialize (its `_view` becomes available).
    browser::wait_for_js(&page, "document.querySelector('crap-richtext')?._view").await;

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

    // Wait until the inserted text lands in the hidden textarea, confirming the
    // insert/select dispatch was applied before clicking bold.
    browser::wait_for_js(
        &page,
        "(document.querySelector('crap-richtext textarea')?.value ?? '').includes('bold text')",
    )
    .await;

    // Click the bold button in the shadow root toolbar
    page.evaluate(
        "() => document.querySelector('crap-richtext').shadowRoot.querySelector('[data-cmd=\"bold\"]').click()",
    )
    .await
    .unwrap();

    // Wait for the hidden textarea (light DOM) to reflect the <strong> markup.
    browser::wait_for_js(
        &page,
        "(document.querySelector('crap-richtext textarea')?.value ?? '').includes('<strong>')",
    )
    .await;

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

// ── richtext_toolbar_edit_marks_the_form_dirty ───────────────────────────

/// Regression: the editor wrote the hidden textarea without firing any
/// event, so a toolbar command (no keystroke reaches the form) left the
/// unsaved-changes guard unaware and the edit could be lost without a prompt.
#[tokio::test(flavor = "multi_thread")]
async fn richtext_toolbar_edit_marks_the_form_dirty() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_richtext_def(), make_users_def()],
        vec![],
        "brt4@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/articles/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    browser::wait_for_js(&page, "document.querySelector('crap-richtext')?._view").await;
    browser::wait_for_js(
        &page,
        "document.querySelector('crap-dirty-form')?._armed === true",
    )
    .await;

    page.evaluate(
        "() => document.querySelector('crap-richtext').shadowRoot.querySelector('[data-cmd=\"hr\"]').click()",
    )
    .await
    .unwrap();

    assert!(
        browser::wait_for_js(
            &page,
            "document.querySelector('crap-dirty-form')?._dirty === true"
        )
        .await,
        "a toolbar edit must mark the form dirty"
    );

    server_handle.abort();
}

// ── richtext_parses_stored_html_inertly ──────────────────────────────────

/// Regression: stored HTML was parsed via `innerHTML` on an element of the
/// live document, where an `<img onerror>` in the stored value runs. The
/// default CSP happens to block inline handlers, so the test turns it off to
/// see the parser itself: it must build an inert document.
#[tokio::test(flavor = "multi_thread")]
async fn richtext_parses_stored_html_inertly() {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;
    config.admin.dev_mode = true;
    config.admin.csp.enabled = false;

    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        app: _app,
        ..
    } = setup_browser_test_with_config(
        vec![make_richtext_def(), make_users_def()],
        vec![],
        config,
        "brt5@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/articles/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    browser::wait_for_js(&page, "customElements.get('crap-richtext')").await;

    // Mount a fresh editor over a hostile stored value and record whether
    // the image's error handler fires.
    page.evaluate(
        "() => { \
            window.__richtextPwned = false; \
            window.__pwn = () => { window.__richtextPwned = true; }; \
            const host = document.createElement('crap-richtext'); \
            const ta = document.createElement('textarea'); \
            ta.value = '<p>hi</p><img src=\"x:broken\" onerror=\"window.__pwn()\">'; \
            host.appendChild(ta); \
            document.body.appendChild(host); \
        }",
    )
    .await
    .unwrap();

    browser::wait_for_js(
        &page,
        "document.querySelectorAll('crap-richtext')[1]?._view",
    )
    .await;
    sleep(Duration::from_millis(300)).await;

    let pwned: bool = page
        .evaluate("() => window.__richtextPwned")
        .await
        .unwrap()
        .into_value()
        .unwrap();
    assert!(!pwned, "stored HTML must not run event handlers");

    server_handle.abort();
}
