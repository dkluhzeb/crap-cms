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

use crap_cms::{
    config::{CrapConfig, LocaleConfig},
    core::{collection::*, field::LocalizedString},
};
use crap_cms_e2e::{browser, helpers::*};

fn make_locale_config() -> CrapConfig {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;
    config.locale = LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    };
    config
}

fn make_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def
}

// ── ui_locale_picker_renders_with_multiple_locales ───────────────────────
//
// The `<crap-ui-locale-picker>` in `templates/layout/header.hbs` is only
// rendered when `available_locales` is non-empty (multiple admin UI
// locales configured). Verifies it appears when configured, including
// the toggle button and a dropdown item per locale.

#[tokio::test(flavor = "multi_thread")]
async fn ui_locale_picker_renders_with_multiple_locales() {
    let config = make_locale_config();
    let (base_url, server_handle, app) =
        browser::spawn_server_with_config(vec![make_def(), make_users_def()], vec![], config).await;
    let user_id = create_test_user(&app, "bln1@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "bln1@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "bln1@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/posts"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    sleep(Duration::from_millis(500)).await;

    let has_picker: bool = page
        .evaluate("() => !!document.querySelector('crap-ui-locale-picker')")
        .await
        .unwrap()
        .into_value()
        .unwrap_or(false);
    assert!(
        has_picker,
        "<crap-ui-locale-picker> should be present with multiple locales"
    );

    let has_toggle: bool = page
        .evaluate("() => !!document.querySelector('[data-ui-locale-toggle]')")
        .await
        .unwrap()
        .into_value()
        .unwrap_or(false);
    assert!(has_toggle, "ui locale toggle button should be present");

    let item_count = page
        .evaluate("() => document.querySelectorAll('[data-ui-locale-value]').length")
        .await
        .unwrap()
        .into_value::<i64>()
        .unwrap_or(0);
    assert_eq!(
        item_count, 2,
        "should have one dropdown item per configured locale (en, de), got {item_count}"
    );

    server_handle.abort();
}

// ── ui_locale_dropdown_opens_on_toggle ───────────────────────────────────
//
// Clicking the toggle button should add the `--open` class to the
// dropdown via `CrapPickerBase`. Verifies the open/close behavior at
// the JS layer without requiring a real POST + reload.

#[tokio::test(flavor = "multi_thread")]
async fn ui_locale_dropdown_opens_on_toggle() {
    let config = make_locale_config();
    let (base_url, server_handle, app) =
        browser::spawn_server_with_config(vec![make_def(), make_users_def()], vec![], config).await;
    let user_id = create_test_user(&app, "bln2@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "bln2@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "bln2@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/posts"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    sleep(Duration::from_millis(500)).await;

    let initially_open: bool = page
        .evaluate(
            "() => document.querySelector('[data-ui-locale-dropdown]')\
                ?.classList.contains('locale-picker__dropdown--open') ?? false",
        )
        .await
        .unwrap()
        .into_value()
        .unwrap_or(false);
    assert!(!initially_open, "dropdown should start closed");

    page.evaluate("() => document.querySelector('[data-ui-locale-toggle]')?.click()")
        .await
        .unwrap();
    sleep(Duration::from_millis(200)).await;

    let opened: bool = page
        .evaluate(
            "() => document.querySelector('[data-ui-locale-dropdown]')\
                ?.classList.contains('locale-picker__dropdown--open') ?? false",
        )
        .await
        .unwrap()
        .into_value()
        .unwrap_or(false);
    assert!(opened, "dropdown should be open after clicking toggle");

    server_handle.abort();
}
