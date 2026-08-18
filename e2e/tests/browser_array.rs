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
    core::{collection::*, field::*},
    db::DbConnection,
};

use crap_cms_e2e::{BrowserTestCtx, browser, helpers::*, setup_browser_test};

fn make_array_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("teams");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Team".to_string())),
        plural: Some(LocalizedString::Plain("Teams".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("name", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("members", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("member_name", FieldType::Text).build(),
            ])
            .build(),
    ];
    def
}

// ── 28. add_row_button_creates_row ────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn add_row_button_creates_row() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_array_def(), make_users_def()],
        vec![],
        "badd@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/teams/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Initially no rows
    let rows = page.find_elements(".form__array-row").await.unwrap();
    assert_eq!(rows.len(), 0, "should start with 0 rows");

    // Wait for the array web component to upgrade before clicking — its
    // `connectedCallback` attaches the `add-array-row` click handler, so under
    // load a click before it's defined does nothing (no row is added).
    browser::wait_for_js(&page, "customElements.get('crap-array-field')").await;

    // Click add
    page.find_element("button[data-action=\"add-array-row\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap();

    // Poll for the cloned row instead of a fixed sleep — the row is inserted by
    // an async DOM update that can outrun a fixed delay under load.
    let rows = browser::wait_for_element_count(&page, ".form__array-row", 1).await;
    assert_eq!(rows.len(), 1, "should have 1 row after clicking add");

    server_handle.abort();
}

// ── 29. remove_row_button_removes_row ─────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn remove_row_button_removes_row() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_array_def(), make_users_def()],
        vec![],
        "brem@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/teams/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Wait for the array component to upgrade before clicking (its
    // connectedCallback wires the add-row handler; a click before it is defined
    // is a no-op under load).
    browser::wait_for_js(&page, "customElements.get('crap-array-field')").await;

    // Add 2 rows. The first find_element uses the post-nav retry helper
    // because chromiumoxide can transiently see a stale frame just after
    // `wait_for_navigation()` returns; subsequent loops are fine.
    browser::find_element_after_nav(&page, "button[data-action=\"add-array-row\"]")
        .await
        .click()
        .await
        .unwrap();
    browser::wait_for_element_count(&page, ".form__array-row", 1).await;

    page.find_element("button[data-action=\"add-array-row\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap();
    let rows = browser::wait_for_element_count(&page, ".form__array-row", 2).await;
    assert_eq!(rows.len(), 2, "should have 2 rows");

    // Remove first row
    page.find_element("button[data-action=\"remove-array-row\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap();

    let rows = browser::wait_for_element_count(&page, ".form__array-row", 1).await;
    assert_eq!(rows.len(), 1, "should have 1 row after removal");

    server_handle.abort();
}

// ── 30. reorder_rows_updates_indices ──────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn reorder_rows_updates_indices() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_array_def(), make_users_def()],
        vec![],
        "breorder@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/teams/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Wait for the array component to upgrade before clicking (handler is wired
    // in its connectedCallback).
    browser::wait_for_js(&page, "customElements.get('crap-array-field')").await;

    // Add 2 rows and fill them. First iteration uses the post-nav
    // retry helper to absorb the brief stale-frame window after
    // `wait_for_navigation()` returns.
    browser::find_element_after_nav(&page, "button[data-action=\"add-array-row\"]")
        .await
        .click()
        .await
        .unwrap();
    browser::wait_for_element_count(&page, ".form__array-row", 1).await;

    page.find_element("button[data-action=\"add-array-row\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap();
    browser::wait_for_element_count(&page, ".form__array-row", 2).await;

    // Type into first row
    let inputs = page
        .find_elements("input[name*=\"member_name\"]")
        .await
        .unwrap();
    assert_eq!(inputs.len(), 2);
    inputs[0]
        .click()
        .await
        .unwrap()
        .type_str("First")
        .await
        .unwrap();
    inputs[1]
        .click()
        .await
        .unwrap()
        .type_str("Second")
        .await
        .unwrap();

    // Click move-down on first row
    page.find_element("button[data-action=\"move-row-down\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap();

    // After reorder, the first row's input should now hold "Second". Poll for
    // the swap instead of a fixed sleep, and actually assert the reordered
    // value (the old test only re-counted the inputs, which never changed).
    let mut first_value = String::new();
    for _ in 0..60 {
        first_value = page
            .evaluate(
                "() => document.querySelectorAll('input[name*=\"member_name\"]')[0]?.value ?? ''",
            )
            .await
            .unwrap()
            .into_value::<String>()
            .unwrap_or_default();
        if first_value == "Second" {
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        first_value, "Second",
        "after move-down, the first row should show the row that was second",
    );

    server_handle.abort();
}

// ── Regression: array rows persist after form submission ─────────────────

#[tokio::test(flavor = "multi_thread")]
async fn array_rows_persist_after_save() {
    let BrowserTestCtx {
        app,
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_array_def(), make_users_def()],
        vec![],
        "barrsave@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/teams/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Fill name. Use the retry helper rather than a fixed sleep —
    // `wait_for_navigation` returns when the navigation event fires,
    // but the create form's DOM may not yet be queryable. The other
    // tests in this file get away with a fixed sleep because their
    // first interaction is `find_elements` (plural, returns empty Vec
    // when nothing matches), not `find_element` (singular, errors
    // with `Could not find node` on a not-yet-rendered DOM).
    browser::find_element_after_nav(&page, "input[name=\"name\"]")
        .await
        .click()
        .await
        .unwrap()
        .type_str("Test Team")
        .await
        .unwrap();

    // Wait for the array component to upgrade before clicking add.
    browser::wait_for_js(&page, "customElements.get('crap-array-field')").await;

    // Add 2 rows and fill them
    for i in 0..2 {
        browser::find_element_after_nav(&page, "button[data-action=\"add-array-row\"]")
            .await
            .click()
            .await
            .unwrap();
        // Wait for the new row to exist before setting its value (a fixed sleep
        // could set the value on a not-yet-cloned row and lose it).
        browser::wait_for_element_count(&page, ".form__array-row", i + 1).await;

        let selector = format!("input[name=\"members[{i}][member_name]\"]");
        page.evaluate(format!(
            "() => {{ const el = document.querySelector('{}'); if (el) {{ el.focus(); el.value = 'Member {}'; }} }}",
            selector, i + 1
        ))
        .await
        .unwrap();
    }

    // Submit
    page.evaluate("() => document.querySelector('#edit-form')?.requestSubmit()")
        .await
        .unwrap();

    // Poll the DB until the save lands instead of a fixed 2s sleep — the submit
    // round-trips through the server, so the write appears asynchronously.
    let conn = app.pool.get().unwrap();
    let mut rows = Vec::new();
    for _ in 0..60 {
        rows = conn
            .query_all("SELECT member_name FROM teams_members ORDER BY _order", &[])
            .unwrap();
        if rows.len() == 2 {
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(rows.len(), 2, "should have 2 array rows saved");

    server_handle.abort();
}
