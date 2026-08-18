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
use crap_cms::{
    config::{CrapConfig, LocaleConfig},
    core::{collection::*, field::*},
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

fn make_localized_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("articles");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Article".to_string())),
        plural: Some(LocalizedString::Plain("Articles".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .localized(true)
            .build(),
        FieldDefinition::builder("slug", FieldType::Text)
            .required(true)
            .build(),
    ];
    def
}

// ── locale_picker_switches_locale ────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn locale_picker_switches_locale() {
    let config = make_locale_config();
    let (base_url, server_handle, app) = browser::spawn_server_with_config(
        vec![make_localized_def(), make_users_def()],
        vec![],
        config,
    )
    .await;
    let user_id = create_test_user(&app, "blocale@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "blocale@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "blocale@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/articles/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Wait for the locale picker component to render
    browser::wait_for_element(&page, "crap-locale-picker").await;

    // The locale picker should be visible (since locales are enabled)
    let pickers = page.find_elements("crap-locale-picker").await.unwrap();
    assert!(
        !pickers.is_empty(),
        "locale picker should be present when locales are enabled"
    );

    // Open the locale picker dropdown
    page.find_element("[data-locale-toggle]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap();

    // Wait for the dropdown option to render before selecting it
    browser::wait_for_element(&page, "[data-locale-value=\"de\"]").await;

    // Click the "de" locale option
    page.find_element("[data-locale-value=\"de\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap();

    // Clicking triggers a POST + reload that sets the cookie; poll for it
    assert!(
        browser::wait_for_js(&page, "document.cookie.includes('crap_editor_locale=de')").await,
        "crap_editor_locale cookie should be set to 'de'"
    );

    // After clicking, a cookie should be set and page reloaded
    // Check the locale badge shows "de"
    let result = page
        .evaluate("() => document.cookie.includes('crap_editor_locale=de')")
        .await
        .unwrap();
    let has_cookie: bool = result.into_value().unwrap_or(false);
    assert!(
        has_cookie,
        "crap_editor_locale cookie should be set to 'de'"
    );

    server_handle.abort();
}
