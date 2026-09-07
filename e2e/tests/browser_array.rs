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

// ── array row identity is stable across an edit ───────────────────────────

/// End-to-end through the real browser: editing an existing array row updates
/// it IN PLACE (keeps its junction-row `id`) rather than the old
/// delete-and-reinsert that minted a fresh id. Proves the hidden row-id input
/// renders, survives the JS, round-trips through the form submit, and drives the
/// diff-based writer — the admin-surface end of the row-identity fix.
#[tokio::test(flavor = "multi_thread")]
async fn array_edit_updates_row_in_place_keeping_its_id() {
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
        "barrid@test.com",
        "pass123",
    )
    .await;

    // --- Create a team with one member via the browser ---
    page.goto(format!("{base_url}/admin/collections/teams/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    browser::find_element_after_nav(&page, "input[name=\"name\"]")
        .await
        .click()
        .await
        .unwrap()
        .type_str("Squad")
        .await
        .unwrap();

    browser::wait_for_js(&page, "customElements.get('crap-array-field')").await;
    browser::find_element_after_nav(&page, "button[data-action=\"add-array-row\"]")
        .await
        .click()
        .await
        .unwrap();
    browser::wait_for_element_count(&page, ".form__array-row", 1).await;

    page.evaluate(
        "() => { const el = document.querySelector('input[name=\"members[0][member_name]\"]'); \
         if (el) { el.focus(); el.value = 'Alice'; } }",
    )
    .await
    .unwrap();
    page.evaluate("() => document.querySelector('#edit-form')?.requestSubmit()")
        .await
        .unwrap();

    // Capture the created member row's id and the parent document id.
    let conn = app.pool.get().unwrap();
    let mut member_id = String::new();
    let mut doc_id = String::new();
    for _ in 0..60 {
        let rows = conn.query_all("SELECT id FROM teams_members", &[]).unwrap();
        if rows.len() == 1 {
            member_id = rows[0].get_string("id").unwrap();
            doc_id = conn
                .query_one("SELECT id FROM teams", &[])
                .unwrap()
                .unwrap()
                .get_string("id")
                .unwrap();
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    assert!(!member_id.is_empty(), "member row should have been created");

    // --- Open the edit form; the hidden row-id input must carry the stored id ---
    page.goto(format!("{base_url}/admin/collections/teams/{doc_id}"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    browser::wait_for_js(&page, "customElements.get('crap-array-field')").await;

    // Wait for the existing row's field to render before reading the hidden id.
    browser::find_element_after_nav(&page, "input[name=\"members[0][member_name]\"]").await;

    let hidden_id: String = page
        .evaluate("() => document.querySelector('input[name=\"members[0][id]\"]')?.value ?? ''")
        .await
        .unwrap()
        .into_value()
        .unwrap();
    assert_eq!(
        hidden_id, member_id,
        "the edit form must round-trip the stored row id as a hidden input"
    );

    // --- Edit the member name and save ---
    page.evaluate(
        "() => { const el = document.querySelector('input[name=\"members[0][member_name]\"]'); \
         if (el) { el.focus(); el.value = 'Alice Updated'; } }",
    )
    .await
    .unwrap();
    page.evaluate("() => document.querySelector('#edit-form')?.requestSubmit()")
        .await
        .unwrap();

    // The row must be UPDATED IN PLACE: same id, new name.
    let mut verified = false;
    for _ in 0..60 {
        let rows = conn
            .query_all("SELECT id, member_name FROM teams_members", &[])
            .unwrap();
        if rows.len() == 1
            && rows[0].get_string("member_name").ok().as_deref() == Some("Alice Updated")
        {
            assert_eq!(
                rows[0].get_string("id").unwrap(),
                member_id,
                "the edited row keeps its id — updated in place, not delete+reinserted"
            );
            verified = true;
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    assert!(verified, "edit must persist and preserve the row id");

    server_handle.abort();
}

// ── duplicate_row_copies_the_current_select_choice ────────────────────────

/// A `<select>` changed but not yet saved keeps its choice in the duplicate:
/// `cloneNode` copies input/textarea values but not select selectedness,
/// so the copy would otherwise revert to the server-rendered option.
fn make_array_select_def() -> CollectionDefinition {
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
                FieldDefinition::builder("role", FieldType::Select)
                    .options(vec![
                        SelectOption::new(LocalizedString::Plain("Dev".into()), "dev"),
                        SelectOption::new(LocalizedString::Plain("Ops".into()), "ops"),
                    ])
                    .build(),
            ])
            .build(),
    ];
    def
}

#[tokio::test(flavor = "multi_thread")]
async fn duplicate_row_copies_the_current_select_choice() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_array_select_def(), make_users_def()],
        vec![],
        "bdup@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/teams/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    browser::wait_for_js(&page, "customElements.get('crap-array-field')").await;

    page.find_element("button[data-action=\"add-array-row\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap();
    browser::wait_for_element_count(&page, ".form__array-row", 1).await;

    // Change the row's select to a non-default option (unsaved), then duplicate.
    page.evaluate(
        "() => { const s = document.querySelector('.form__array-row select'); s.value = 'ops'; s.dispatchEvent(new Event('change', { bubbles: true })); }",
    )
    .await
    .unwrap();
    page.find_element("button[data-action=\"duplicate-row\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap();
    browser::wait_for_element_count(&page, ".form__array-row", 2).await;

    let values = page
        .evaluate(
            "() => Array.from(document.querySelectorAll('.form__array-row select')).map((s) => s.value).join(',')",
        )
        .await
        .unwrap();
    let values: String = values.into_value().unwrap();
    assert_eq!(
        values, "ops,ops",
        "the duplicate keeps the changed select choice"
    );

    server_handle.abort();
}
