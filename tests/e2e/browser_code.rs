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

// ── code_language_picker_persists_choice ─────────────────────────────────
//
// When `admin.languages` is configured, the form renders a `<select>`
// inside the `<crap-code>` shadow root and a hidden `<input name="..._lang">`
// next to it. Choosing a language must update the hidden input AND
// reconfigure the editor's language compartment so the highlighting follows.

fn make_picker_def() -> CollectionDefinition {
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
        FieldDefinition::builder("code", FieldType::Code)
            .admin(
                FieldAdmin::builder()
                    .language("javascript")
                    .languages(vec!["javascript".to_string(), "python".to_string()])
                    .build(),
            )
            .build(),
    ];
    def
}

#[tokio::test(flavor = "multi_thread")]
async fn code_language_picker_persists_choice() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_picker_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "bcode4@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "bcode4@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "bcode4@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/snippets/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Picker is in the shadow root; hidden input is a sibling of <crap-code>.
    let picker_state = browser::shadow_eval(
        &page,
        "crap-code",
        "const sel = root.querySelector('select.lang-picker__select'); \
         return sel ? sel.value : 'missing';",
    )
    .await;
    assert_eq!(
        picker_state, "javascript",
        "default language is the operator default"
    );

    let initial_hidden = page
        .evaluate(
            "() => document.querySelector('input[type=hidden][name=\"code_lang\"]')?.value ?? ''",
        )
        .await
        .unwrap()
        .into_value::<String>()
        .unwrap();
    assert_eq!(initial_hidden, "javascript");

    // Change the picker to "python".
    page.evaluate(
        "() => { \
            const sel = document.querySelector('crap-code').shadowRoot \
                .querySelector('select.lang-picker__select'); \
            sel.value = 'python'; \
            sel.dispatchEvent(new Event('change', { bubbles: true })); \
        }",
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let after_hidden = page
        .evaluate(
            "() => document.querySelector('input[type=hidden][name=\"code_lang\"]')?.value ?? ''",
        )
        .await
        .unwrap()
        .into_value::<String>()
        .unwrap();
    assert_eq!(
        after_hidden, "python",
        "picker change must update the hidden _lang input"
    );

    server_handle.abort();
}

// ── code_picker_appears_inside_blocks_after_add ───────────────────────────
//
// Mirrors the projects example. Defines a `code_block` block-definition
// that contains a code field with `admin.languages`. After adding a row,
// the cloned `<crap-code>` shadow root must render the language picker.

fn make_blocks_picker_def() -> CollectionDefinition {
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
        FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![BlockDefinition {
                block_type: "code_block".to_string(),
                fields: vec![
                    FieldDefinition::builder("code", FieldType::Code)
                        .admin(
                            FieldAdmin::builder()
                                .language("javascript")
                                .languages(vec!["javascript".to_string(), "python".to_string()])
                                .build(),
                        )
                        .build(),
                ],
                label: Some(LocalizedString::Plain("Code".to_string())),
                ..Default::default()
            }])
            .build(),
    ];
    def
}

#[tokio::test(flavor = "multi_thread")]
async fn code_picker_appears_inside_blocks_after_add() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_blocks_picker_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "bcode5@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "bcode5@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "bcode5@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/snippets/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Click the "Add Code" button — the block-picker dispatches add-block-row.
    page.evaluate(
        "() => { \
            const btn = document.querySelector('[data-action=\"add-block-row\"]'); \
            btn.click(); \
        }",
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // The newly cloned <crap-code> should render its picker in the shadow root.
    let picker_present = browser::shadow_eval(
        &page,
        "crap-code",
        "const sel = root.querySelector('select.lang-picker__select'); \
         return sel ? `${sel.options.length} options` : 'missing';",
    )
    .await;
    assert!(
        picker_present.contains("options") && !picker_present.contains("0 options"),
        "expected picker with options inside the cloned block row, got: {picker_present}"
    );

    // The hidden `_lang` input must also be present in the cloned row (sibling
    // of <crap-code> in light DOM).
    let hidden_present = page
        .evaluate(
            "() => document.querySelector('input[type=hidden][name$=\"_lang\"]') ? 'present' : 'missing'",
        )
        .await
        .unwrap()
        .into_value::<String>()
        .unwrap();
    assert_eq!(hidden_present, "present");

    server_handle.abort();
}
