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

use chromiumoxide::cdp::browser_protocol::emulation::{
    SetDeviceMetricsOverrideParams, SetTouchEmulationEnabledParams,
};
use tokio::time::sleep;

use crap_cms::core::{collection::*, field::*};

use crap_cms_e2e::{browser, helpers::*};

fn make_sidebar_def() -> CollectionDefinition {
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

// ── sidebar_toggle_opens_closes ──────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn sidebar_toggle_opens_closes() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_sidebar_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "bside@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "bside@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    // Set mobile viewport via CDP so the sidebar toggle is visible
    page.execute(
        SetDeviceMetricsOverrideParams::builder()
            .mobile(true)
            .width(375)
            .height(812)
            .device_scale_factor(2.)
            .build()
            .unwrap(),
    )
    .await
    .unwrap();
    page.execute(SetTouchEmulationEnabledParams::new(true))
        .await
        .unwrap();

    browser::browser_login(&page, &base_url, "bside@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/posts"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Sidebar should not be open initially on mobile
    let result = page
        .evaluate("() => document.querySelector('.sidebar')?.classList.contains('sidebar--open') ?? false")
        .await
        .unwrap();
    let is_open: bool = result.into_value().unwrap();
    assert!(!is_open, "sidebar should be closed initially on mobile");

    // Click hamburger toggle via JS (may be visibility-dependent on viewport)
    page.evaluate("() => document.querySelector('[data-action=\"toggle-sidebar\"]')?.click()")
        .await
        .unwrap();
    sleep(Duration::from_millis(300)).await;

    // Sidebar should now be open
    let result = page
        .evaluate("() => document.querySelector('.sidebar').classList.contains('sidebar--open')")
        .await
        .unwrap();
    let is_open: bool = result.into_value().unwrap();
    assert!(is_open, "sidebar should be open after clicking toggle");

    // Click toggle again to close
    page.evaluate("() => document.querySelector('[data-action=\"toggle-sidebar\"]')?.click()")
        .await
        .unwrap();
    sleep(Duration::from_millis(300)).await;

    // Sidebar should be closed again
    let result = page
        .evaluate("() => document.querySelector('.sidebar').classList.contains('sidebar--open')")
        .await
        .unwrap();
    let is_open: bool = result.into_value().unwrap();
    assert!(
        !is_open,
        "sidebar should be closed after clicking toggle again"
    );

    server_handle.abort();
}
