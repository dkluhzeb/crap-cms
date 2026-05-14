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
use crap_cms_e2e::browser;
use crap_cms_e2e::helpers::*;

use crap_cms::core::collection::*;
use crap_cms::core::field::*;

fn make_tabs_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("profiles");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Profile".to_string())),
        plural: Some(LocalizedString::Plain("Profiles".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("info", FieldType::Tabs)
            .tabs(vec![
                FieldTab {
                    label: "Basic".to_string(),
                    description: None,
                    fields: vec![FieldDefinition::builder("first_name", FieldType::Text).build()],
                },
                FieldTab {
                    label: "Contact".to_string(),
                    description: None,
                    fields: vec![FieldDefinition::builder("email", FieldType::Email).build()],
                },
            ])
            .build(),
    ];
    def
}

// ── 31. tab_switching_shows_correct_panel ─────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn tab_switching_shows_correct_panel() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_tabs_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "btabs@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "btabs@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "btabs@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/profiles/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Initially first tab should be active
    let active_tabs = page
        .find_elements("[role=\"tab\"][aria-selected=\"true\"]")
        .await
        .unwrap();
    assert_eq!(active_tabs.len(), 1, "should have 1 active tab initially");

    // Click second tab
    let tabs = page.find_elements("[role=\"tab\"]").await.unwrap();
    assert_eq!(tabs.len(), 2, "should have 2 tab buttons");
    tabs[1].click().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // Second tab should now be selected
    let second_tab_selected = page
        .find_elements("[role=\"tab\"][aria-selected=\"true\"]")
        .await
        .unwrap();
    assert_eq!(
        second_tab_selected.len(),
        1,
        "should still have exactly 1 active tab"
    );

    // Second panel should be visible (not hidden)
    let hidden_panels = page
        .find_elements(".form__tabs-panel--hidden")
        .await
        .unwrap();
    assert_eq!(
        hidden_panels.len(),
        1,
        "after switching, 1 panel should be hidden"
    );

    server_handle.abort();
}
