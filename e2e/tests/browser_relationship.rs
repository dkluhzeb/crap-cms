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
use std::{collections::HashMap, time::Duration};

use serde_json::json;
use tokio::time::sleep;

use crap_cms::{
    core::{DocumentFields, collection::*, field::*},
    db::{DbConnection, query},
};

use crap_cms_e2e::{browser, helpers::*};

fn make_categories_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("categories");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Category".to_string())),
        plural: Some(LocalizedString::Plain("Categories".to_string())),
    };
    def.timestamps = true;
    def.admin = AdminConfig {
        use_as_title: Some("name".to_string()),
        ..Default::default()
    };
    def.fields = vec![
        FieldDefinition::builder("name", FieldType::Text)
            .required(true)
            .build(),
    ];
    def
}

fn make_rel_posts_def() -> CollectionDefinition {
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
        FieldDefinition::builder("category", FieldType::Relationship)
            .relationship(RelationshipConfig::new("categories", false))
            .build(),
        FieldDefinition::builder("tags", FieldType::Relationship)
            .relationship(RelationshipConfig::new("categories", true))
            .has_many(true)
            .build(),
    ];
    def
}

fn create_category(app: &TestApp, name: &str) -> String {
    let def = app.registry.get_collection("categories").unwrap().clone();

    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields = HashMap::from([("name".to_string(), json!(name))]).into();
    let doc = query::create(&tx, "categories", &def, &data, None).unwrap();
    tx.commit().unwrap();
    doc.id.to_string()
}

// ── relationship_search_shows_results ────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn relationship_search_shows_results() {
    let (base_url, server_handle, app) = browser::spawn_server(
        vec![
            make_categories_def(),
            make_rel_posts_def(),
            make_users_def(),
        ],
        vec![],
    )
    .await;
    let user_id = create_test_user(&app, "brel1@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "brel1@test.com");

    create_category(&app, "Technology");
    create_category(&app, "Science");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "brel1@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/posts/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    // Wait for the relationship input to mount before focusing it.
    browser::wait_for_element(&page, ".relationship-search__input").await;

    // Focus the has-one relationship input to trigger a search (shows all results)
    page.evaluate(
        "() => { \
            const input = document.querySelector('.relationship-search__input'); \
            if (input) input.focus(); \
        }",
    )
    .await
    .unwrap();
    // Wait for debounce + fetch to populate the dropdown (assert wants >= 2).
    browser::wait_for_js(
        &page,
        "document.querySelectorAll('.relationship-search__option').length >= 2",
    )
    .await;

    // Dropdown should appear with results
    let result = page
        .evaluate("() => document.querySelectorAll('.relationship-search__option').length")
        .await
        .unwrap();
    let option_count: i64 = result.into_value().unwrap_or(0);
    assert!(
        option_count >= 2,
        "should show search results in dropdown, got {option_count} options"
    );

    server_handle.abort();
}

// ── relationship_select_sets_value ───────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn relationship_select_sets_value() {
    let (base_url, server_handle, app) = browser::spawn_server(
        vec![
            make_categories_def(),
            make_rel_posts_def(),
            make_users_def(),
        ],
        vec![],
    )
    .await;
    let user_id = create_test_user(&app, "brel2@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "brel2@test.com");

    create_category(&app, "Music");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "brel2@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/posts/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    // Wait for the relationship input to mount before focusing it.
    browser::wait_for_element(&page, ".relationship-search__input").await;

    // Focus the has-one input to trigger initial search
    page.evaluate("() => document.querySelector('.relationship-search__input')?.focus()")
        .await
        .unwrap();
    // Wait for the dropdown options to load before clicking one.
    browser::wait_for_element(&page, ".relationship-search__option").await;

    // Click the first option via mousedown (how the component listens)
    page.evaluate(
        "() => { \
            const opt = document.querySelector('.relationship-search__option'); \
            if (opt) opt.dispatchEvent(new MouseEvent('mousedown', {bubbles: true})); \
        }",
    )
    .await
    .unwrap();
    // Wait for the hidden input to receive the selected value.
    browser::wait_for_js(
        &page,
        "document.querySelector('.relationship-search__hidden input[type=hidden]')?.value",
    )
    .await;

    // Hidden input should have a value
    let result = page
        .evaluate(
            "() => document.querySelector('.relationship-search__hidden input[type=\"hidden\"]')?.value ?? ''",
        )
        .await
        .unwrap();
    let hidden_val: String = result.into_value().unwrap();
    assert!(
        !hidden_val.is_empty(),
        "hidden input should have a value after selection"
    );

    server_handle.abort();
}

// ── relationship_has_many_multiple_chips ──────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn relationship_has_many_multiple_chips() {
    let (base_url, server_handle, app) = browser::spawn_server(
        vec![
            make_categories_def(),
            make_rel_posts_def(),
            make_users_def(),
        ],
        vec![],
    )
    .await;
    let user_id = create_test_user(&app, "brel3@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "brel3@test.com");

    create_category(&app, "Alpha");
    create_category(&app, "Beta");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "brel3@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/posts/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    // Wait for the has-many component to mount before focusing its input.
    browser::wait_for_element(&page, "crap-relationship-search[has-many]").await;

    // Focus the has-many input (tags field) to trigger search
    page.evaluate(
        "() => { \
            const el = document.querySelector('crap-relationship-search[has-many]'); \
            const input = el?.querySelector('.relationship-search__tags-input') \
                       || el?.querySelector('.relationship-search__input'); \
            if (input) input.focus(); \
        }",
    )
    .await
    .unwrap();
    // Wait for the dropdown options to load.
    browser::wait_for_element(
        &page,
        "crap-relationship-search[has-many] .relationship-search__option",
    )
    .await;

    // Select first option
    page.evaluate(
        "() => { \
            const opt = document.querySelectorAll('crap-relationship-search[has-many] .relationship-search__option')[0]; \
            if (opt) opt.dispatchEvent(new MouseEvent('mousedown', {bubbles: true})); \
        }",
    )
    .await
    .unwrap();
    // Wait for the first chip to register.
    browser::wait_for_element_count(&page, ".pill-list__chip", 1).await;

    // Wait for dropdown to close after first selection
    browser::wait_for_element_gone(
        &page,
        "crap-relationship-search[has-many] .relationship-search__option",
    )
    .await;

    // Type a space then delete it to trigger input event which triggers search
    page.evaluate(
        "() => { \
            const el = document.querySelector('crap-relationship-search[has-many]'); \
            const input = el?.querySelector('.relationship-search__tags-input') \
                       || el?.querySelector('.relationship-search__input'); \
            if (input) { \
                input.focus(); \
                input.value = ''; \
                input.dispatchEvent(new Event('input', {bubbles: true})); \
            } \
        }",
    )
    .await
    .unwrap();
    // Wait for the dropdown options to repopulate after re-triggering search.
    browser::wait_for_element(
        &page,
        "crap-relationship-search[has-many] .relationship-search__option",
    )
    .await;

    // Select the unselected option
    page.evaluate(
        "() => { \
            const el = document.querySelector('crap-relationship-search[has-many]'); \
            const opts = el?.querySelectorAll('.relationship-search__option') || []; \
            for (const opt of opts) { \
                if (!opt.classList.contains('relationship-search__option--selected')) { \
                    opt.dispatchEvent(new MouseEvent('mousedown', {bubbles: true})); \
                    break; \
                } \
            } \
        }",
    )
    .await
    .unwrap();
    // Wait for the second chip to register.
    browser::wait_for_element_count(&page, ".pill-list__chip", 2).await;

    // Should have 2 chips
    let result = page
        .evaluate("() => document.querySelectorAll('.pill-list__chip').length")
        .await
        .unwrap();
    let chip_count: i64 = result.into_value().unwrap_or(0);
    assert!(
        chip_count >= 2,
        "should have at least 2 chips for has-many, got {chip_count}"
    );

    server_handle.abort();
}

// ── relationship_remove_chip ─────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn relationship_remove_chip() {
    let (base_url, server_handle, app) = browser::spawn_server(
        vec![
            make_categories_def(),
            make_rel_posts_def(),
            make_users_def(),
        ],
        vec![],
    )
    .await;
    let user_id = create_test_user(&app, "brel4@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "brel4@test.com");

    create_category(&app, "RemoveMe");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "brel4@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/posts/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    // Wait for the has-many component to mount before focusing its input.
    browser::wait_for_element(&page, "crap-relationship-search[has-many]").await;

    // Focus the has-many input to trigger search
    page.evaluate(
        "() => { \
            const el = document.querySelector('crap-relationship-search[has-many]'); \
            const input = el?.querySelector('.relationship-search__tags-input') \
                       || el?.querySelector('.relationship-search__input'); \
            if (input) input.focus(); \
        }",
    )
    .await
    .unwrap();
    // Wait for the dropdown options to load before selecting one.
    browser::wait_for_element(
        &page,
        "crap-relationship-search[has-many] .relationship-search__option",
    )
    .await;

    // Select first option
    page.evaluate(
        "() => { \
            const opt = document.querySelector('crap-relationship-search[has-many] .relationship-search__option'); \
            if (opt) opt.dispatchEvent(new MouseEvent('mousedown', {bubbles: true})); \
        }",
    )
    .await
    .unwrap();
    // Wait for the chip to appear.
    browser::wait_for_element_count(&page, ".pill-list__chip", 1).await;

    // Should have a chip
    let result = page
        .evaluate("() => document.querySelectorAll('.pill-list__chip').length")
        .await
        .unwrap();
    let chips_before: i64 = result.into_value().unwrap_or(0);
    assert!(chips_before > 0, "should have a chip after selecting");

    // Click remove on the chip
    page.evaluate("() => document.querySelector('.pill-list__chip-remove')?.click()")
        .await
        .unwrap();
    // Wait for the chip to be removed.
    browser::wait_for_element_gone(&page, ".pill-list__chip").await;

    let result = page
        .evaluate("() => document.querySelectorAll('.pill-list__chip').length")
        .await
        .unwrap();
    let chips_after: i64 = result.into_value().unwrap_or(0);
    assert_eq!(chips_after, 0, "chip should be removed after clicking X");

    server_handle.abort();
}

// ── Regression: Enter key selects first search result ────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn relationship_enter_selects_first_result() {
    let (base_url, server_handle, app) = browser::spawn_server(
        vec![
            make_categories_def(),
            make_rel_posts_def(),
            make_users_def(),
        ],
        vec![],
    )
    .await;
    let user_id = create_test_user(&app, "benter@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "benter@test.com");

    create_category(&app, "EnterTest");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "benter@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/posts/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    // Wait for the has-many component to mount before focusing its input.
    browser::wait_for_element(&page, "crap-relationship-search[has-many]").await;

    // Focus has-many input and type to trigger search
    page.evaluate(
        "() => { \
            const el = document.querySelector('crap-relationship-search[has-many]'); \
            const input = el?.querySelector('.relationship-search__tags-input') \
                       || el?.querySelector('.relationship-search__input'); \
            if (input) { input.focus(); input.value = 'Enter'; \
                input.dispatchEvent(new Event('input', {bubbles: true})); } \
        }",
    )
    .await
    .unwrap();

    // Wait up to 3s for the search dropdown to populate (debounce + network).
    let mut options = 0i64;
    for _ in 0..30 {
        sleep(Duration::from_millis(100)).await;
        options = page
            .evaluate(
                "() => document.querySelectorAll('crap-relationship-search[has-many] .relationship-search__option').length",
            )
            .await
            .unwrap()
            .into_value()
            .unwrap_or(0);
        if options >= 1 {
            break;
        }
    }
    assert!(options >= 1, "should have search results, got {options}");

    // Press Enter without using arrow keys — should select first result
    page.evaluate(
        "() => { \
            const el = document.querySelector('crap-relationship-search[has-many]'); \
            const input = el?.querySelector('.relationship-search__tags-input') \
                       || el?.querySelector('.relationship-search__input'); \
            if (input) input.dispatchEvent(new KeyboardEvent('keydown', {key: 'Enter', bubbles: true})); \
        }",
    )
    .await
    .unwrap();
    // Wait for the chip produced by the Enter selection.
    browser::wait_for_element_count(&page, ".pill-list__chip", 1).await;

    // Should have a chip
    let result = page
        .evaluate("() => document.querySelectorAll('.pill-list__chip').length")
        .await
        .unwrap();
    let chips: i64 = result.into_value().unwrap_or(0);
    assert!(
        chips >= 1,
        "Enter should select first result as chip, got {chips} chips"
    );

    server_handle.abort();
}

// ── Regression: has-one relationship persists after save ─────────────────

#[tokio::test(flavor = "multi_thread")]
async fn has_one_relationship_persists() {
    let (base_url, server_handle, app) = browser::spawn_server(
        vec![
            make_categories_def(),
            make_rel_posts_def(),
            make_users_def(),
        ],
        vec![],
    )
    .await;
    let user_id = create_test_user(&app, "bpersist@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "bpersist@test.com");

    let cat_id = create_category(&app, "Persisted");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "bpersist@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/posts/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    // Wait for the relationship input to mount.
    browser::wait_for_element(&page, ".relationship-search__input").await;

    // Fill title
    page.find_element("input[name=\"title\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap()
        .type_str("Persist Test")
        .await
        .unwrap();

    // Focus relationship input to trigger search
    page.evaluate("() => document.querySelector('.relationship-search__input')?.focus()")
        .await
        .unwrap();
    // Wait for the dropdown options to load before selecting one.
    browser::wait_for_element(&page, ".relationship-search__option").await;

    // Select first option
    page.evaluate(
        "() => { \
            const opt = document.querySelector('.relationship-search__option'); \
            if (opt) opt.dispatchEvent(new MouseEvent('mousedown', {bubbles: true})); \
        }",
    )
    .await
    .unwrap();
    // Wait for the has-one hidden input to receive the selected value.
    browser::wait_for_js(
        &page,
        "document.querySelector('crap-relationship-search:not([has-many]) input[type=hidden]')?.value",
    )
    .await;

    // Submit form
    page.evaluate("() => document.querySelector('#edit-form')?.requestSubmit()")
        .await
        .unwrap();

    // Poll the DB until the post row lands (submit is async).
    let mut rows = Vec::new();
    for _ in 0..60 {
        let conn = app.pool.get().unwrap();
        rows = conn
            .query_all(
                "SELECT category FROM posts WHERE title = 'Persist Test'",
                &[],
            )
            .unwrap();
        if !rows.is_empty() {
            break;
        }
        drop(conn);
        sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(rows.len(), 1, "should have created one post");

    let saved_cat = rows[0]
        .get_value(0)
        .and_then(|v| match v {
            crap_cms::db::DbValue::Text(s) => Some(s.clone()),
            _ => None,
        })
        .unwrap_or_default();
    assert_eq!(
        saved_cat, cat_id,
        "saved category should match the selected one"
    );

    server_handle.abort();
}

// ── Regression: has-many relationship persists after save ────────────────

#[tokio::test(flavor = "multi_thread")]
async fn has_many_relationship_persists() {
    let (base_url, server_handle, app) = browser::spawn_server(
        vec![
            make_categories_def(),
            make_rel_posts_def(),
            make_users_def(),
        ],
        vec![],
    )
    .await;
    let user_id = create_test_user(&app, "bhasmany@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "bhasmany@test.com");

    let cat_a_id = create_category(&app, "HasMany A");
    let cat_b_id = create_category(&app, "HasMany B");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "bhasmany@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/posts/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    // Wait for the has-many component to mount.
    browser::wait_for_element(&page, "crap-relationship-search[has-many]").await;

    // Fill title
    page.find_element("input[name=\"title\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap()
        .type_str("HasMany Test")
        .await
        .unwrap();

    // Focus has-many input (tags field) to trigger search
    page.evaluate(
        "() => { \
            const el = document.querySelector('crap-relationship-search[has-many]'); \
            const input = el?.querySelector('.relationship-search__tags-input') \
                       || el?.querySelector('.relationship-search__input'); \
            if (input) input.focus(); \
        }",
    )
    .await
    .unwrap();
    // Wait for the dropdown options to load.
    browser::wait_for_element(
        &page,
        "crap-relationship-search[has-many] .relationship-search__option",
    )
    .await;

    // Select first option
    page.evaluate(
        "() => { \
            const opt = document.querySelector('crap-relationship-search[has-many] .relationship-search__option'); \
            if (opt) opt.dispatchEvent(new MouseEvent('mousedown', {bubbles: true})); \
        }",
    )
    .await
    .unwrap();
    // Wait for the first chip to register.
    browser::wait_for_element_count(&page, ".pill-list__chip", 1).await;

    // Re-trigger search to select second option
    page.evaluate(
        "() => { \
            const el = document.querySelector('crap-relationship-search[has-many]'); \
            const input = el?.querySelector('.relationship-search__tags-input') \
                       || el?.querySelector('.relationship-search__input'); \
            if (input) { input.focus(); input.value = ''; \
                input.dispatchEvent(new Event('input', {bubbles: true})); } \
        }",
    )
    .await
    .unwrap();
    // Wait for the dropdown options to repopulate.
    browser::wait_for_element(
        &page,
        "crap-relationship-search[has-many] .relationship-search__option",
    )
    .await;

    // Select the unselected option
    page.evaluate(
        "() => { \
            const el = document.querySelector('crap-relationship-search[has-many]'); \
            const opts = el?.querySelectorAll('.relationship-search__option') || []; \
            for (const opt of opts) { \
                if (!opt.classList.contains('relationship-search__option--selected')) { \
                    opt.dispatchEvent(new MouseEvent('mousedown', {bubbles: true})); \
                    break; \
                } \
            } \
        }",
    )
    .await
    .unwrap();
    // Wait for the second chip to register.
    browser::wait_for_element_count(&page, ".pill-list__chip", 2).await;

    // Should have 2 chips
    let result = page
        .evaluate("() => document.querySelectorAll('.pill-list__chip').length")
        .await
        .unwrap();
    let chips: i64 = result.into_value().unwrap_or(0);
    assert!(chips >= 2, "should have 2 chips, got {chips}");

    // Submit form
    page.evaluate("() => document.querySelector('#edit-form')?.requestSubmit()")
        .await
        .unwrap();

    // Poll the DB until both join-table references land (submit is async).
    let mut rows = Vec::new();
    for _ in 0..60 {
        let conn = app.pool.get().unwrap();
        rows = conn
            .query_all("SELECT related_id FROM posts_tags ORDER BY related_id", &[])
            .unwrap();
        if rows.len() == 2 {
            break;
        }
        drop(conn);
        sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        rows.len(),
        2,
        "should have 2 has-many references saved in join table"
    );

    // Verify the saved IDs match
    let mut saved_ids: Vec<String> = rows
        .iter()
        .filter_map(|r| {
            r.get_value(0).and_then(|v| match v {
                crap_cms::db::DbValue::Text(s) => Some(s.clone()),
                _ => None,
            })
        })
        .collect();
    saved_ids.sort();
    let mut expected = vec![cat_a_id, cat_b_id];
    expected.sort();
    assert_eq!(
        saved_ids, expected,
        "saved tag IDs should match the selected categories"
    );

    server_handle.abort();
}

// ── Regression: inline create panel creates and selects item ─────────────

#[tokio::test(flavor = "multi_thread")]
async fn relationship_inline_create_selects_item() {
    let (base_url, server_handle, app) = browser::spawn_server(
        vec![
            make_categories_def(),
            make_rel_posts_def(),
            make_users_def(),
        ],
        vec![],
    )
    .await;
    let user_id = create_test_user(&app, "binline@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "binline@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "binline@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/posts/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    // Wait for the inline-create control to mount.
    browser::wait_for_element(&page, "[data-inline-create=\"categories\"]").await;

    // Fill post title
    page.find_element("input[name=\"title\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap()
        .type_str("Inline Create Test")
        .await
        .unwrap();

    // Click "Create new" on the has-one category field
    page.evaluate(
        "() => { \
            const link = document.querySelector('[data-inline-create=\"categories\"]'); \
            if (link) { link.click(); } \
        }",
    )
    .await
    .unwrap();
    // Wait for the create panel to open.
    browser::wait_for_js(&page, "document.querySelector('.create-panel')?.open").await;

    // The create panel dialog should be open
    let panel_open = page
        .evaluate(
            "() => { \
                const dialog = document.querySelector('.create-panel'); \
                return dialog && dialog.open ? 'open' : 'closed'; \
            }",
        )
        .await
        .unwrap();
    let state: String = panel_open.into_value().unwrap_or_default();
    assert_eq!(state, "open", "create panel should be open");

    // The create form is fetched and mounted ASYNCHRONOUSLY after the dialog
    // opens (see create-panel.js), so wait for its name input to exist before
    // filling it — otherwise the fill targets a not-yet-mounted form and the
    // submit finds no form (the panel never closes).
    browser::wait_for_js(
        &page,
        "document.querySelector('.create-panel__body input[name=name]')",
    )
    .await;

    // Fill the category name in the panel form
    page.evaluate(
        "() => { \
            const input = document.querySelector('.create-panel__body input[name=\"name\"]'); \
            if (input) { input.focus(); input.value = 'NewInlineCategory'; \
                input.dispatchEvent(new Event('input', {bubbles: true})); } \
        }",
    )
    .await
    .unwrap();
    // Wait for the name value to be applied before submitting.
    browser::wait_for_js(
        &page,
        "document.querySelector('.create-panel__body input[name=name]')?.value === 'NewInlineCategory'",
    )
    .await;

    page.evaluate(
        "() => { \
            const form = document.querySelector('.create-panel__body form'); \
            if (form) form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })); \
        }",
    )
    .await
    .unwrap();

    // Poll up to 6s for the panel to close (fetch + create + DB).
    let mut state2 = String::new();
    for _ in 0..60 {
        sleep(Duration::from_millis(100)).await;
        state2 = page
            .evaluate(
                "() => { \
                    const dialog = document.querySelector('.create-panel'); \
                    return (!dialog || !dialog.open) ? 'closed' : 'open'; \
                }",
            )
            .await
            .unwrap()
            .into_value()
            .unwrap_or_default();
        if state2 == "closed" {
            break;
        }
    }
    assert_eq!(
        state2, "closed",
        "create panel should close after successful creation"
    );

    // The has-one hidden input should now have the created category's ID
    let hidden_val = page
        .evaluate(
            "() => { \
                const hidden = document.querySelector('crap-relationship-search:not([has-many]) input[type=\"hidden\"]'); \
                return hidden?.value || ''; \
            }",
        )
        .await
        .unwrap();
    let selected_id: String = hidden_val.into_value().unwrap_or_default();
    assert!(
        !selected_id.is_empty(),
        "created category should be auto-selected in the relationship field"
    );

    // Verify the category was actually created in the database
    let conn = app.pool.get().unwrap();
    let rows = conn
        .query_all(
            "SELECT name FROM categories WHERE name = 'NewInlineCategory'",
            &[],
        )
        .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "the inline-created category should exist in the database"
    );

    server_handle.abort();
}

// ── relationship_inline_create_validation_error_rerenders ────────────────
//
// Regression test for the inline-create panel's validation-error path.
//
// `_submitForm` previously declared `let body;` inside the async function,
// shadowing the function parameter `body` that pointed at the panel-body
// `<div>`. After the fetch the line `this._injectForm(body, html, ...)`
// passed the SHADOWED inner `body` (`URLSearchParams` or `FormData`) into
// `_injectForm`, where `body.replaceChildren(...)` immediately threw
// `TypeError: body.replaceChildren is not a function`. Validation errors
// from the server thus crashed the panel instead of re-rendering the form.
//
// This test triggers a validation error (empty `name` on a required field)
// and asserts the panel re-renders with the form still mounted — proof
// that `_injectForm` ran to completion, which was impossible with the bug.

#[tokio::test(flavor = "multi_thread")]
async fn relationship_inline_create_validation_error_rerenders() {
    let (base_url, server_handle, app) = browser::spawn_server(
        vec![
            make_categories_def(),
            make_rel_posts_def(),
            make_users_def(),
        ],
        vec![],
    )
    .await;
    let user_id = create_test_user(&app, "binline_err@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "binline_err@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();
    browser::browser_login(&page, &base_url, "binline_err@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/posts/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    // Wait for the inline-create control to mount.
    browser::wait_for_element(&page, "[data-inline-create=\"categories\"]").await;

    // Open the inline-create panel for categories.
    page.evaluate(
        "() => { \
            const link = document.querySelector('[data-inline-create=\"categories\"]'); \
            if (link) { link.click(); } \
        }",
    )
    .await
    .unwrap();
    // Wait for the panel form to render before submitting it.
    browser::wait_for_element(&page, ".create-panel__body form").await;

    // Submit the form with the required `name` left empty — this triggers
    // a 422 from the server and exercises the re-render path.
    page.evaluate(
        "() => { \
            const form = document.querySelector('.create-panel__body form'); \
            if (form) form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })); \
        }",
    )
    .await
    .unwrap();

    // Poll up to 6s for the form to be re-rendered. With the bug,
    // `_injectForm` throws and the form is never replaced — the input
    // attribute used as the marker would still be present from the
    // initial render. With the fix the form is replaced; the input is
    // present in the new HTML too. The discriminator is whether the
    // _server-rendered_ validation error element exists on the field.
    let mut has_error = false;
    for _ in 0..60 {
        sleep(Duration::from_millis(100)).await;
        let result = page
            .evaluate(
                "() => { \
                    const body = document.querySelector('.create-panel__body'); \
                    if (!body) return 'no-body'; \
                    if (!body.querySelector('form')) return 'no-form'; \
                    return body.querySelector('.form__error, [data-validate-error]') ? 'error' : 'no-error'; \
                }",
            )
            .await
            .unwrap();
        let state: String = result.into_value().unwrap_or_default();
        if state == "error" {
            has_error = true;
            break;
        }
    }

    let panel_state: String = page
        .evaluate(
            "() => { \
                const dialog = document.querySelector('.create-panel'); \
                return dialog && dialog.open ? 'open' : 'closed'; \
            }",
        )
        .await
        .unwrap()
        .into_value()
        .unwrap_or_default();
    assert_eq!(
        panel_state, "open",
        "panel should stay open on validation error"
    );
    assert!(
        has_error,
        "validation-error response should re-render the form with an error message; \
         a missing error element means `_injectForm` never ran (the body shadowing bug)"
    );

    server_handle.abort();
}
