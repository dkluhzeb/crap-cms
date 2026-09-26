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
    core::{
        collection::*,
        field::{FieldDefinition, FieldType, LocalizedString},
    },
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

    // Wait for the UI locale picker component to render
    browser::wait_for_element(&page, "crap-ui-locale-picker").await;

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

    // Wait for the dropdown element to render before reading/clicking
    browser::wait_for_element(&page, "[data-ui-locale-dropdown]").await;

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

    // Poll until the --open class is added to the dropdown
    assert!(
        browser::wait_for_js(
            &page,
            "document.querySelector('[data-ui-locale-dropdown]')\
                ?.classList.contains('locale-picker__dropdown--open') ?? false"
        )
        .await,
        "dropdown should be open after clicking toggle"
    );

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

// ── the_editor_locale_waits_for_the_unsaved_changes_answer ──────────────
//
// Regression: the editor locale picker wrote its cookie and THEN reloaded,
// so the unsaved-changes prompt came after the switch was already recorded
// — "Stay" kept the old-locale form on screen with the cookie pointing at
// the new locale, and the save redirected into the other locale. The cookie
// is written only once the editor chose to leave.

#[tokio::test(flavor = "multi_thread")]
async fn the_editor_locale_waits_for_the_unsaved_changes_answer() {
    let mut def = make_def();
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .localized(true)
            .build(),
    ];

    let config = make_locale_config();
    let (base_url, server_handle, app) =
        browser::spawn_server_with_config(vec![def, make_users_def()], vec![], config).await;
    let user_id = create_test_user(&app, "bln9@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "bln9@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();
    browser::browser_login(&page, &base_url, "bln9@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/posts/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    assert!(
        browser::wait_for_js(
            &page,
            "document.querySelector('crap-dirty-form')?._armed === true \
             && !!document.querySelector('crap-locale-picker')"
        )
        .await,
        "the edit form and the locale picker are ready"
    );

    page.evaluate(
        "() => { \
           const el = document.querySelector('[name=\"title\"]'); \
           el.value = 'Unsaved'; \
           el.dispatchEvent(new Event('input', { bubbles: true })); \
           document.querySelector('crap-locale-picker')._onValue('de'); \
         }",
    )
    .await
    .unwrap();

    assert!(
        browser::wait_for_js(
            &page,
            "document.querySelector('crap-confirm-dialog')?.shadowRoot?.querySelector('dialog')?.open === true"
        )
        .await,
        "the unsaved-changes question is asked"
    );

    let cookie_while_asking: String = page
        .evaluate("() => document.cookie")
        .await
        .unwrap()
        .into_value()
        .unwrap_or_default();
    assert!(
        !cookie_while_asking.contains("crap_editor_locale=de"),
        "the locale is not switched before the answer: {cookie_while_asking}"
    );

    page.evaluate(
        "() => document.querySelector('crap-confirm-dialog').shadowRoot.querySelector('.cancel').click()",
    )
    .await
    .unwrap();

    let cookie_after_stay: String = page
        .evaluate("() => document.cookie")
        .await
        .unwrap()
        .into_value()
        .unwrap_or_default();
    assert!(
        !cookie_after_stay.contains("crap_editor_locale=de"),
        "\"Stay\" keeps the locale: {cookie_after_stay}"
    );

    server_handle.abort();
}
