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
use std::{collections::HashMap, time::Duration};

use serde_json::json;
use tokio::time::sleep;

use crap_cms::{
    config::CrapConfig,
    core::{DocumentFields, collection::*, field::*},
    db::{DbConnection, DbValue, query},
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

// ── richtext_unloadable_json_is_kept_read_only ───────────────────────────

/// Regression: a stored JSON document holding a node the field's editor does
/// not have (a feature disabled after the content was written) loaded as an
/// EMPTY editor, and the first edit overwrote the stored value. The editor now
/// shows an error and keeps the textarea value untouched, read-only: the form
/// resubmits it exactly as stored.
#[tokio::test(flavor = "multi_thread")]
async fn richtext_unloadable_json_is_kept_read_only() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_richtext_def(), make_users_def()],
        vec![],
        "brt6@test.com",
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

    // Only `bold` enabled: the stored heading cannot be loaded.
    page.evaluate(
        r#"() => {
            const host = document.createElement('crap-richtext');
            host.id = 'rt-unloadable';
            host.setAttribute('data-format', 'json');
            host.setAttribute('data-features', '["bold"]');
            const ta = document.createElement('textarea');
            ta.value = '{"type":"doc","content":[{"type":"heading","attrs":{"level":1},"content":[{"type":"text","text":"Keep me"}]}]}';
            host.appendChild(ta);
            document.body.appendChild(host);
        }"#,
    )
    .await
    .unwrap();

    assert!(
        browser::wait_for_js(
            &page,
            "document.querySelector('#rt-unloadable')?.shadowRoot?.querySelector('.richtext__load-error')"
        )
        .await,
        "an unloadable document must show the load error"
    );

    let state: String = page
        .evaluate(
            "() => { const h = document.querySelector('#rt-unloadable'); \
             const ta = h.querySelector('textarea'); \
             return [String(!!h._view), String(ta.disabled), String(ta.readOnly), ta.value, \
                     h.shadowRoot.querySelector('.richtext__load-error-source').textContent].join('|'); }",
        )
        .await
        .unwrap()
        .into_value()
        .unwrap();
    let stored = r#"{"type":"doc","content":[{"type":"heading","attrs":{"level":1},"content":[{"type":"text","text":"Keep me"}]}]}"#;
    assert_eq!(state, format!("false|false|true|{stored}|{stored}"));

    server_handle.abort();
}

// ── richtext_drops_disallowed_link_schemes ───────────────────────────────

/// Regression: the link protocol allowlist guarded only the link dialog; a
/// pasted or stored `<a href="javascript:…">` became a link mark and was
/// written back into the value.
#[tokio::test(flavor = "multi_thread")]
async fn richtext_drops_disallowed_link_schemes() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_richtext_def(), make_users_def()],
        vec![],
        "brt7@test.com",
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

    page.evaluate(
        r#"() => {
            const host = document.createElement('crap-richtext');
            host.id = 'rt-links';
            const ta = document.createElement('textarea');
            ta.value = '<p><a href="java&#9;script:alert(1)">bad</a> <a href="https://example.com">good</a></p>';
            host.appendChild(ta);
            document.body.appendChild(host);
        }"#,
    )
    .await
    .unwrap();
    browser::wait_for_js(&page, "document.querySelector('#rt-links')?._view").await;

    // Any edit re-serializes the document into the textarea.
    page.evaluate(
        "() => { const v = document.querySelector('#rt-links')._view; \
         v.dispatch(v.state.tr.insertText('!', v.state.doc.content.size - 1)); }",
    )
    .await
    .unwrap();
    browser::wait_for_js(
        &page,
        "(document.querySelector('#rt-links textarea')?.value ?? '').includes('!')",
    )
    .await;

    let value: String = page
        .evaluate("() => document.querySelector('#rt-links textarea').value")
        .await
        .unwrap()
        .into_value()
        .unwrap();
    assert!(!value.contains("script:"), "disallowed link kept: {value}");
    assert!(value.contains("bad"), "the link text stays: {value}");
    assert!(value.contains(r#"href="https://example.com""#), "{value}");

    server_handle.abort();
}

// ── richtext_shows_placeholder_while_empty ───────────────────────────────

/// Regression: `admin.placeholder` was rendered only on the hidden textarea,
/// so the editor never showed it. The editable root carries it while the
/// document is empty and drops it once there is content.
#[tokio::test(flavor = "multi_thread")]
async fn richtext_shows_placeholder_while_empty() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_richtext_def(), make_users_def()],
        vec![],
        "brt8@test.com",
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

    page.evaluate(
        r"() => {
            const host = document.createElement('crap-richtext');
            host.id = 'rt-placeholder';
            const ta = document.createElement('textarea');
            ta.placeholder = 'Write here';
            host.appendChild(ta);
            document.body.appendChild(host);
        }",
    )
    .await
    .unwrap();
    browser::wait_for_js(&page, "document.querySelector('#rt-placeholder')?._view").await;

    let empty: String = page
        .evaluate(
            "() => document.querySelector('#rt-placeholder')._view.dom.getAttribute('data-placeholder') ?? ''",
        )
        .await
        .unwrap()
        .into_value()
        .unwrap();
    assert_eq!(empty, "Write here");

    page.evaluate(
        "() => { const v = document.querySelector('#rt-placeholder')._view; \
         v.dispatch(v.state.tr.insertText('x', 1)); }",
    )
    .await
    .unwrap();

    let typed: bool = page
        .evaluate(
            "() => document.querySelector('#rt-placeholder')._view.dom.hasAttribute('data-placeholder')",
        )
        .await
        .unwrap()
        .into_value()
        .unwrap();
    assert!(
        !typed,
        "the placeholder must go once the editor has content"
    );

    server_handle.abort();
}

// ── richtext_json_is_kept_as_stored_until_edited ─────────────────────────

/// Regression: loading a JSON document rewrote the textarea with the document
/// as the editor loaded it (an attribute its schema does not declare
/// dropped), so opening and saving a document changed content nobody edited.
/// An untouched field now submits the value as stored — the server accepts it
/// as held — and the first edit serializes the document as loaded.
#[tokio::test(flavor = "multi_thread")]
async fn richtext_json_is_kept_as_stored_until_edited() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_richtext_def(), make_users_def()],
        vec![],
        "brt9@test.com",
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

    page.evaluate(
        r#"() => {
            const host = document.createElement('crap-richtext');
            host.id = 'rt-normalized';
            host.setAttribute('data-format', 'json');
            const ta = document.createElement('textarea');
            ta.value = '{"type":"doc","content":[{"type":"paragraph","attrs":{"align":"left"},"content":[{"type":"text","text":"Kept"}]}]}';
            host.appendChild(ta);
            document.body.appendChild(host);
        }"#,
    )
    .await
    .unwrap();
    browser::wait_for_js(&page, "document.querySelector('#rt-normalized')?._view").await;

    let textarea = "() => document.querySelector('#rt-normalized textarea').value";

    let untouched: String = page.evaluate(textarea).await.unwrap().into_value().unwrap();
    assert!(
        untouched.contains("\"align\":\"left\""),
        "an untouched field keeps the stored value: {untouched}"
    );

    page.evaluate(
        "() => { const v = document.querySelector('#rt-normalized')._view; \
         v.dispatch(v.state.tr.insertText('!', 5)); }",
    )
    .await
    .unwrap();

    let edited: String = page.evaluate(textarea).await.unwrap().into_value().unwrap();
    assert!(
        !edited.contains("align"),
        "an edit drops the stale attr: {edited}"
    );
    assert!(edited.contains("Kept!"), "{edited}");

    server_handle.abort();
}

// ── richtext_nested_unloadable_value_survives_a_save ─────────────────────

/// A JSON rich text field inside a group inside a block row, enabling only
/// `bold` — so a stored heading cannot be loaded.
fn make_nested_richtext_def() -> CollectionDefinition {
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
            .blocks(vec![BlockDefinition::new(
                "section",
                vec![
                    FieldDefinition::builder("meta", FieldType::Group)
                        .fields(vec![
                            FieldDefinition::builder("body", FieldType::Richtext)
                                .admin(
                                    FieldAdmin::builder()
                                        .richtext_format("json")
                                        .features(vec!["bold".to_string()])
                                        .build(),
                                )
                                .build(),
                        ])
                        .build(),
                ],
            )])
            .build(),
    ];
    def
}

/// Store a page whose block row holds `body` directly, bypassing validation —
/// the value was written before its feature was disabled.
fn seed_page(app: &TestApp, body: &str) -> String {
    let def = app.registry.get_collection("pages").unwrap().clone();

    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields = HashMap::from([("title".to_string(), json!("Before"))]).into();
    let doc = query::create(&tx, "pages", &def, &data, None).unwrap();
    let row = json!({ "_block_type": "section", "meta": { "body": body } });
    query::set_block_rows(&tx, "pages", "content", &doc.id, &[row], None).unwrap();
    tx.commit().unwrap();

    doc.id.to_string()
}

/// Regression: an unloadable value nested in a group inside a block row was
/// left out of the submission (its textarea was disabled), and a row stored as
/// JSON keeps only the keys it is sent — so saving the page for an unrelated
/// edit silently dropped the value. It is now resubmitted unchanged and the
/// server accepts the value the page already holds.
#[tokio::test(flavor = "multi_thread")]
async fn richtext_nested_unloadable_value_survives_a_save() {
    let BrowserTestCtx {
        app,
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_nested_richtext_def(), make_users_def()],
        vec![],
        "brt10@test.com",
        "pass123",
    )
    .await;

    let body = r#"{"type":"doc","content":[{"type":"heading","attrs":{"level":1},"content":[{"type":"text","text":"Keep me"}]}]}"#;
    let id = seed_page(&app, body);

    page.goto(format!("{base_url}/admin/collections/pages/{id}"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    assert!(
        browser::wait_for_js(
            &page,
            "document.querySelector('crap-richtext')?.shadowRoot?.querySelector('.richtext__load-error')"
        )
        .await,
        "the nested unloadable document must show the load error"
    );

    page.evaluate(
        "() => { const input = document.querySelector('input[name=\"title\"]'); \
         input.value = 'After'; input.dispatchEvent(new Event('input', {bubbles: true})); \
         document.querySelector('#edit-form').requestSubmit(); }",
    )
    .await
    .unwrap();

    let conn = app.pool.get().unwrap();
    let mut title = String::new();
    for _ in 0..60 {
        title = conn
            .query_one(
                "SELECT title FROM pages WHERE id = ?1",
                &[DbValue::Text(id.clone())],
            )
            .unwrap()
            .and_then(|row| row.get_string("title").ok())
            .unwrap_or_default();
        if title == "After" {
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(title, "After", "the save must land");

    let data = conn
        .query_one(
            "SELECT data FROM pages_content WHERE parent_id = ?1",
            &[DbValue::Text(id)],
        )
        .unwrap()
        .and_then(|row| row.get_string("data").ok())
        .unwrap_or_default();
    assert!(
        data.contains("Keep me") && data.contains("heading"),
        "the nested value must survive the save: {data}"
    );

    server_handle.abort();
}
