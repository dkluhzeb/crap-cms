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

use crap_cms_e2e::{BrowserTestCtx, helpers::*, setup_browser_test};

fn make_theme_def() -> CollectionDefinition {
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

// ── theme_picker_changes_data_attribute ──────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn theme_picker_changes_data_attribute() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_theme_def(), make_users_def()],
        vec![],
        "btheme1@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/posts"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Open theme picker dropdown
    page.find_element("[data-theme-toggle]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap();
    sleep(Duration::from_millis(300)).await;

    // Select "tokyo-night" theme
    page.find_element("[data-theme-value=\"tokyo-night\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap();
    sleep(Duration::from_millis(300)).await;

    // Check that data-theme is set on <html>
    let result = page
        .evaluate("() => document.documentElement.getAttribute('data-theme')")
        .await
        .unwrap();
    let theme: String = result.into_value().unwrap();
    assert_eq!(
        theme, "tokyo-night",
        "data-theme should be 'tokyo-night' on <html>"
    );

    server_handle.abort();
}

// ── theme_persists_across_navigation ─────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn theme_persists_across_navigation() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_theme_def(), make_users_def()],
        vec![],
        "btheme2@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/posts"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Set theme via JS (equivalent to the picker)
    page.evaluate("() => { window.crap.theme.set('gruvbox'); }")
        .await
        .unwrap();
    sleep(Duration::from_millis(200)).await;

    // Navigate to create page
    page.goto(format!("{base_url}/admin/collections/posts/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    sleep(Duration::from_millis(300)).await;

    // Theme should persist (read from localStorage and applied on load)
    let result = page
        .evaluate("() => document.documentElement.getAttribute('data-theme')")
        .await
        .unwrap();
    let theme: String = result.into_value().unwrap();
    assert_eq!(theme, "gruvbox", "theme should persist across navigation");

    server_handle.abort();
}
