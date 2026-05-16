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

fn make_dirty_def() -> CollectionDefinition {
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

// ── dirty_form_not_armed_on_clean ────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn dirty_form_not_armed_on_clean() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_dirty_def(), make_users_def()],
        vec![],
        "bdirty1@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/posts/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Wait for the component to arm itself (requestAnimationFrame)
    sleep(Duration::from_millis(500)).await;

    // Without any interaction, _dirty should be false
    let result = page
        .evaluate("() => { const df = document.querySelector('crap-dirty-form'); return df ? df._dirty : null; }")
        .await
        .unwrap();
    let dirty: bool = result.into_value().unwrap_or(false);
    assert!(!dirty, "dirty form should not be dirty on a clean page");

    server_handle.abort();
}

// ── dirty_form_armed_after_input ─────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn dirty_form_armed_after_input() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_dirty_def(), make_users_def()],
        vec![],
        "bdirty2@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/posts/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Wait for arming
    sleep(Duration::from_millis(500)).await;

    // Type into the title field
    page.find_element("input[name=\"title\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap()
        .type_str("Some title")
        .await
        .unwrap();
    sleep(Duration::from_millis(300)).await;

    // _dirty should now be true
    let result = page
        .evaluate("() => { const df = document.querySelector('crap-dirty-form'); return df ? df._dirty : null; }")
        .await
        .unwrap();
    let dirty: bool = result.into_value().unwrap_or(false);
    assert!(
        dirty,
        "dirty form should be dirty after typing into a field"
    );

    server_handle.abort();
}
