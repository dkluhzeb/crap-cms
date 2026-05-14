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
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_dirty_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "bdirty1@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "bdirty1@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "bdirty1@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/posts/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Wait for the component to arm itself (requestAnimationFrame)
    tokio::time::sleep(Duration::from_millis(500)).await;

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
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_dirty_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "bdirty2@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "bdirty2@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "bdirty2@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/posts/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Wait for arming
    tokio::time::sleep(Duration::from_millis(500)).await;

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
    tokio::time::sleep(Duration::from_millis(300)).await;

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
