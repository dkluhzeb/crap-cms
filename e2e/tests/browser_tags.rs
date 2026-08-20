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
use crap_cms::core::{collection::*, field::*};

use crap_cms_e2e::{BrowserTestCtx, browser, helpers::*, setup_browser_test};

fn make_tags_def() -> CollectionDefinition {
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
        FieldDefinition::builder("keywords", FieldType::Text)
            .has_many(true)
            .build(),
        // Scalar has-many `Number`: the column is TEXT (JSON array), and the
        // list round-trips as numbers through the admin form.
        FieldDefinition::builder("scores", FieldType::Number)
            .has_many(true)
            .build(),
    ];
    def
}

/// Drive the `<crap-tags>` shadow input by setting `.value` and dispatching
/// a synthetic `keydown` Enter — the component's listener mutates state from
/// either path. We can't use `page.find_element(".tags__input")` because the
/// element lives in Shadow DOM (closed-from-CSS-perspective for `querySelector`).
async fn add_tag(page: &chromiumoxide::Page, value: &str) {
    let js = format!(
        "() => {{ \
            const host = document.querySelector('crap-tags'); \
            const input = host.shadowRoot.querySelector('.tags__input'); \
            input.focus(); \
            input.value = {value}; \
            input.dispatchEvent(new Event('input')); \
            input.dispatchEvent(new KeyboardEvent('keydown', {{ key: 'Enter', bubbles: true }})); \
            return 'ok'; \
        }}",
        value = serde_json::to_string(value).unwrap(),
    );

    page.evaluate(js.as_str()).await.unwrap();
}

async fn chip_count(page: &chromiumoxide::Page) -> i64 {
    page.evaluate(
        "() => document.querySelector('crap-tags').shadowRoot.querySelectorAll('.chip').length",
    )
    .await
    .unwrap()
    .into_value()
    .unwrap_or(0)
}

// ── tags_add_via_enter ───────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn tags_add_via_enter() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_tags_def(), make_users_def()],
        vec![],
        "btag1@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/articles/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    add_tag(&page, "rust").await;

    // Poll (via shadow root JS) until the chip is rendered.
    browser::wait_for_js(
        &page,
        "document.querySelector('crap-tags').shadowRoot.querySelectorAll('.chip').length === 1",
    )
    .await;

    assert_eq!(
        chip_count(&page).await,
        1,
        "should have 1 tag chip after pressing Enter"
    );

    server_handle.abort();
}

// ── tags_remove_via_click ────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn tags_remove_via_click() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_tags_def(), make_users_def()],
        vec![],
        "btag2@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/articles/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    add_tag(&page, "removeme").await;

    // Wait for the chip (and its remove button) to render before clicking.
    browser::wait_for_js(
        &page,
        "document.querySelector('crap-tags').shadowRoot.querySelectorAll('.chip').length === 1",
    )
    .await;

    // Click the chip's remove button via shadow root.
    page.evaluate(
        "() => document.querySelector('crap-tags').shadowRoot.querySelector('.chip__remove').click()",
    )
    .await
    .unwrap();

    // Wait until the chip is gone.
    browser::wait_for_js(
        &page,
        "document.querySelector('crap-tags').shadowRoot.querySelectorAll('.chip').length === 0",
    )
    .await;

    assert_eq!(
        chip_count(&page).await,
        0,
        "chip should be removed after clicking X"
    );

    server_handle.abort();
}

// ── tags_prevent_duplicates ──────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn tags_prevent_duplicates() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_tags_def(), make_users_def()],
        vec![],
        "btag3@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/articles/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    for _ in 0..2 {
        add_tag(&page, "duplicate").await;

        // The first add creates one chip; the second is a no-op — either way
        // the count settles at exactly 1.
        browser::wait_for_js(
            &page,
            "document.querySelector('crap-tags').shadowRoot.querySelectorAll('.chip').length === 1",
        )
        .await;
    }

    assert_eq!(
        chip_count(&page).await,
        1,
        "duplicate tags should be prevented"
    );

    server_handle.abort();
}

/// Drive the `scores` (`Number` has-many) widget — the 2nd `<crap-tags>` on the
/// page (after `keywords`). Same synthetic-Enter path as [`add_tag`].
async fn add_score(page: &chromiumoxide::Page, value: &str) {
    let js = format!(
        "() => {{ \
            const host = document.querySelectorAll('crap-tags')[1]; \
            const input = host.shadowRoot.querySelector('.tags__input'); \
            input.focus(); \
            input.value = {value}; \
            input.dispatchEvent(new Event('input')); \
            input.dispatchEvent(new KeyboardEvent('keydown', {{ key: 'Enter', bubbles: true }})); \
            return 'ok'; \
        }}",
        value = serde_json::to_string(value).unwrap(),
    );

    page.evaluate(js.as_str()).await.unwrap();
}

const SCORE_CHIPS: &str =
    "document.querySelectorAll('crap-tags')[1].shadowRoot.querySelectorAll('.chip').length";

// ── number_has_many_persists_and_reloads ─────────────────────────────────

/// A `Number` has-many list survives the full admin round-trip: submit the
/// create form, follow the redirect to the edit page, and confirm the two
/// numeric chips render from the stored (TEXT/JSON) column. Pre-fix this column
/// was numeric — the write path stored a JSON array that Postgres would reject
/// and the read path returned a raw string the widget couldn't render.
#[tokio::test(flavor = "multi_thread")]
async fn number_has_many_persists_and_reloads() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_tags_def(), make_users_def()],
        vec![],
        "btag5@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/articles/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    page.find_element("input[name=\"title\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap()
        .type_str("Scored Article")
        .await
        .unwrap();

    for (i, score) in ["10", "20"].iter().enumerate() {
        add_score(&page, score).await;

        let expected = i + 1;
        browser::wait_for_js(&page, &format!("{SCORE_CHIPS} === {expected}")).await;
    }

    // Submit and follow the redirect to the new document's edit page.
    page.find_element(".edit-sidebar button[type='submit']")
        .await
        .unwrap()
        .click()
        .await
        .unwrap();
    page.wait_for_navigation().await.unwrap();

    // The stored Number list must re-render as two chips on the edit page.
    assert!(
        browser::wait_for_js(&page, &format!("{SCORE_CHIPS} === 2")).await,
        "the two Number has-many chips should persist and re-render after reload"
    );

    server_handle.abort();
}

// ── tags_submit_persists ─────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn tags_submit_persists() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_tags_def(), make_users_def()],
        vec![],
        "btag4@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/articles/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Title is light-DOM, can use find_element directly.
    page.find_element("input[name=\"title\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap()
        .type_str("Tag Article")
        .await
        .unwrap();

    for (i, tag) in ["alpha", "beta"].iter().enumerate() {
        add_tag(&page, tag).await;

        // Wait for the chip count to reach the expected running total before
        // adding the next tag (shadow-root chips).
        let expected = i + 1;
        browser::wait_for_js(
            &page,
            &format!(
                "document.querySelector('crap-tags').shadowRoot.querySelectorAll('.chip').length === {expected}"
            ),
        )
        .await;
    }

    // Hidden input is in light DOM (slotted from outside the shadow root).
    let hidden_val: String = page
        .evaluate("() => document.querySelector('crap-tags input[type=\"hidden\"]')?.value ?? ''")
        .await
        .unwrap()
        .into_value()
        .unwrap();
    assert!(
        hidden_val.contains("alpha"),
        "hidden input should contain 'alpha', got: {hidden_val}"
    );
    assert!(
        hidden_val.contains("beta"),
        "hidden input should contain 'beta', got: {hidden_val}"
    );

    server_handle.abort();
}
