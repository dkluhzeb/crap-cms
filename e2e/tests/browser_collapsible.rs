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

fn make_collapsible_def() -> CollectionDefinition {
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
        FieldDefinition::builder("details", FieldType::Collapsible)
            .admin(FieldAdminBuilder::new().collapsed(false).build())
            .fields(vec![
                FieldDefinition::builder("subtitle", FieldType::Text).build(),
                FieldDefinition::builder("summary", FieldType::Textarea).build(),
            ])
            .build(),
    ];
    def
}

// ── collapsible_starts_expanded ──────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn collapsible_starts_expanded() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_collapsible_def(), make_users_def()],
        vec![],
        "bcoll1@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/articles/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Wait for JS components to initialize (toggle button is wired up on init)
    browser::wait_for_element(&page, "[data-action=\"toggle-group\"]").await;

    // Collapsible should start expanded (no --collapsed class)
    let collapsed = page
        .find_elements(".form__collapsible--collapsed")
        .await
        .unwrap();
    assert!(
        collapsed.is_empty(),
        "collapsible should start expanded (no --collapsed class)"
    );

    // The toggle button should have aria-expanded="true"
    let result = page
        .evaluate(
            "() => { const el = document.querySelector('[data-action=\"toggle-group\"]'); return el ? el.getAttribute('aria-expanded') : 'NOT_FOUND'; }",
        )
        .await
        .unwrap();
    let expanded: String = result.into_value().unwrap();
    assert_eq!(expanded, "true", "aria-expanded should be true initially");

    server_handle.abort();
}

// ── collapsible_toggles_on_click ─────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn collapsible_toggles_on_click() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_collapsible_def(), make_users_def()],
        vec![],
        "bcoll2@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/articles/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Click toggle to collapse. `click_until_element_count` re-clicks if the
    // first click raced the component upgrade (module-bound handler).
    let collapsed = browser::click_until_element_count(
        &page,
        "[data-action=\"toggle-group\"]",
        ".form__collapsible--collapsed",
        1,
    )
    .await;
    assert_eq!(
        collapsed.len(),
        1,
        "collapsible should be collapsed after clicking toggle"
    );

    // aria-expanded should be false
    let result = page
        .evaluate(
            "() => document.querySelector('[data-action=\"toggle-group\"]').getAttribute('aria-expanded')",
        )
        .await
        .unwrap();
    let expanded: String = result.into_value().unwrap();
    assert_eq!(
        expanded, "false",
        "aria-expanded should be false when collapsed"
    );

    server_handle.abort();
}

// ── collapsible_re_expands ───────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn collapsible_re_expands() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_collapsible_def(), make_users_def()],
        vec![],
        "bcoll3@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/articles/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Collapse (retry-click: the first click can race the component upgrade;
    // once collapsed, the handler is proven bound, so the re-expand below can
    // click plainly).
    browser::click_until_element_count(
        &page,
        "[data-action=\"toggle-group\"]",
        ".form__collapsible--collapsed",
        1,
    )
    .await;

    // Re-expand
    page.find_element("[data-action=\"toggle-group\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap();

    // Should no longer be collapsed — poll until the class is gone
    browser::wait_for_element_gone(&page, ".form__collapsible--collapsed").await;
    let collapsed = page
        .find_elements(".form__collapsible--collapsed")
        .await
        .unwrap();
    assert!(
        collapsed.is_empty(),
        "collapsible should be expanded after toggling twice"
    );

    let result = page
        .evaluate(
            "() => document.querySelector('[data-action=\"toggle-group\"]').getAttribute('aria-expanded')",
        )
        .await
        .unwrap();
    let expanded: String = result.into_value().unwrap();
    assert_eq!(
        expanded, "true",
        "aria-expanded should be true after re-expanding"
    );

    server_handle.abort();
}
