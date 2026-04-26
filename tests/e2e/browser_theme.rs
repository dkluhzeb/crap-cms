use std::time::Duration;

use crate::browser;
use crate::helpers::*;

use crap_cms::core::collection::*;
use crap_cms::core::field::*;

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
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_theme_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "btheme1@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "btheme1@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "btheme1@test.com", "pass123").await;

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
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Select "tokyo-night" theme
    page.find_element("[data-theme-value=\"tokyo-night\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

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
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_theme_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "btheme2@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "btheme2@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "btheme2@test.com", "pass123").await;

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
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Navigate to create page
    page.goto(format!("{base_url}/admin/collections/posts/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Theme should persist (read from localStorage and applied on load)
    let result = page
        .evaluate("() => document.documentElement.getAttribute('data-theme')")
        .await
        .unwrap();
    let theme: String = result.into_value().unwrap();
    assert_eq!(theme, "gruvbox", "theme should persist across navigation");

    server_handle.abort();
}
