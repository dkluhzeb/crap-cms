use std::time::Duration;

use crate::browser;
use crate::helpers::*;

use crap_cms::core::collection::*;
use crap_cms::core::field::*;

fn make_code_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("snippets");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Snippet".to_string())),
        plural: Some(LocalizedString::Plain("Snippets".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("code", FieldType::Code).build(),
    ];
    def
}

// ── code_renders_codemirror ──────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn code_renders_codemirror() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_code_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "bcode1@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "bcode1@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "bcode1@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/snippets/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Check for CodeMirror editor inside shadow root
    let has_editor = browser::shadow_eval(
        &page,
        "crap-code",
        "return root.querySelector('.cm-editor') ? 'true' : 'false';",
    )
    .await;
    assert_eq!(
        has_editor, "true",
        "crap-code shadow root should contain .cm-editor"
    );

    server_handle.abort();
}

// ── code_typing_updates_hidden_input ─────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn code_typing_updates_hidden_input() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_code_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "bcode2@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "bcode2@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "bcode2@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/snippets/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Type into the CodeMirror editor via JS (direct interaction with shadow DOM)
    page.evaluate(
        "() => { \
            const host = document.querySelector('crap-code'); \
            const view = host._view; \
            if (view) { \
                view.dispatch({ changes: { from: 0, insert: 'hello world' } }); \
            } \
        }",
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Check that the hidden textarea has been updated
    let result = page
        .evaluate("() => document.querySelector('crap-code textarea')?.value ?? ''")
        .await
        .unwrap();
    let textarea_val: String = result.into_value().unwrap();
    assert!(
        textarea_val.contains("hello world"),
        "hidden textarea should be updated with typed content, got: {textarea_val}"
    );

    server_handle.abort();
}

// ── code_syntax_highlighting_renders ─────────────────────────────────────
//
// Regression test for CodeMirror syntax highlighting. CM's
// `defaultHighlightStyle` decorates parsed tokens with `.tok-*` classes
// (`.tok-keyword`, `.tok-string`, …); we assert at least one is present
// after typing a snippet of JSON.
//
// Caught by past refactors that reordered the extension array so the
// language extension was registered AFTER the fallback style — meaning
// no language was active and no token classes were emitted.

#[tokio::test(flavor = "multi_thread")]
async fn code_syntax_highlighting_renders() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_code_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "bcode3@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "bcode3@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "bcode3@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/snippets/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Insert a JSON snippet with at least a string and a number — both
    // should be tagged by the parser.
    page.evaluate(
        "() => { \
            const host = document.querySelector('crap-code'); \
            const view = host._view; \
            if (view) { \
                view.dispatch({ changes: { from: 0, insert: '{\"name\":\"hello\",\"n\":42}' } }); \
            } \
        }",
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Look for any highlighted span. CodeMirror's HighlightStyle generates
    // scoped class names (`ͼ` Greek-iota prefix from style-mod). The real
    // test is whether the span has a non-default `color` — meaning the
    // highlight style actually applied.
    let result = browser::shadow_eval(
        &page,
        "crap-code",
        "const line = root.querySelector('.cm-line'); \
         if (!line) return 'no-line'; \
         const span = line.querySelector('span[class*=\"ͼ\"]'); \
         if (!span) return 'no-span'; \
         const color = getComputedStyle(span).color; \
         return color === 'rgb(0, 0, 0)' || color === 'inherit' || color === '' \
           ? 'unstyled' \
           : 'colored';",
    )
    .await;
    assert_eq!(
        result, "colored",
        "expected highlighted tokens to have a non-default color; got: '{result}'"
    );

    server_handle.abort();
}
