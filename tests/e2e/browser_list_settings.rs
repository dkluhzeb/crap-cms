use std::collections::HashMap;
use std::time::Duration;

use crate::browser;
use crate::helpers::*;

use crap_cms::core::collection::*;
use crap_cms::core::field::*;
use crap_cms::db::query;
use crap_cms::db::{DbConnection, DbValue};

fn make_list_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def.timestamps = true;
    def.admin.use_as_title = Some("title".to_string());
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("status", FieldType::Select)
            .options(vec![
                SelectOption::new(LocalizedString::Plain("Draft".to_string()), "draft"),
                SelectOption::new(LocalizedString::Plain("Published".to_string()), "published"),
            ])
            .build(),
        FieldDefinition::builder("views", FieldType::Number).build(),
    ];
    def
}

fn create_list_post(app: &TestApp, title: &str) {
    let reg = app.registry.read().unwrap();
    let def = reg.get_collection("posts").unwrap().clone();
    drop(reg);

    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = HashMap::from([("title".to_string(), title.to_string())]);
    query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();
}

/// Posts collection with drafts enabled (`versions.drafts = true`),
/// so `_status` is exposed as a filterable field in the drawer.
fn make_list_def_with_drafts() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def.timestamps = true;
    def.versions = Some(VersionsConfig::new(true, 10));
    def.admin.use_as_title = Some("title".to_string());
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
    ];
    def
}

/// Seed a row with the system `_status` column set to either `draft`
/// or `published`. Mirrors the seeding pattern in
/// `tests/admin_collections.rs::list_items_status_query_narrows_drafts_only`.
fn create_post_with_system_status(app: &TestApp, title: &str, system_status: &str) {
    let reg = app.registry.read().unwrap();
    let def = reg.get_collection("posts").unwrap().clone();
    drop(reg);

    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = HashMap::from([("title".to_string(), title.to_string())]);
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    if system_status != "published" {
        tx.execute(
            "UPDATE posts SET _status = ?1 WHERE id = ?2",
            &[
                DbValue::Text(system_status.to_string()),
                DbValue::Text(doc.id.to_string()),
            ],
        )
        .unwrap();
    }
    tx.commit().unwrap();
}

fn create_list_post_with_status(app: &TestApp, title: &str, status: &str) {
    let reg = app.registry.read().unwrap();
    let def = reg.get_collection("posts").unwrap().clone();
    drop(reg);

    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = HashMap::from([
        ("title".to_string(), title.to_string()),
        ("status".to_string(), status.to_string()),
    ]);
    query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();
}

// ── column_picker_opens_drawer ───────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn column_picker_opens_drawer() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_list_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "blist1@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "blist1@test.com");

    create_list_post(&app, "Sample Post");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "blist1@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/posts"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    // Wait for JS components to initialize
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Click the "Columns" button
    page.evaluate("() => document.querySelector('[data-action=\"open-column-picker\"]')?.click()")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // The drawer dialog should be open (shadow DOM)
    let result = browser::shadow_eval(
        &page,
        "crap-drawer",
        "return root.querySelector('dialog')?.hasAttribute('open') ? 'true' : 'false';",
    )
    .await;
    assert_eq!(result, "true", "drawer should be open for column picker");

    // Column picker items with checkboxes should be inside the drawer's shadow DOM body
    let checkbox_count = browser::shadow_eval(
        &page,
        "crap-drawer",
        "return String(root.querySelectorAll('.column-picker__item input[type=\"checkbox\"]').length);",
    )
    .await;
    let count: i64 = checkbox_count.parse().unwrap_or(0);
    assert!(
        count > 0,
        "column picker should contain checkboxes, got {count}"
    );

    server_handle.abort();
}

// ── filter_builder_adds_condition ─────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn filter_builder_adds_condition() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_list_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "blist2@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "blist2@test.com");

    create_list_post(&app, "Filterable Post");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "blist2@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/posts"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    // Wait for JS components
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Click "Filters" button
    page.evaluate("() => document.querySelector('[data-action=\"open-filter-builder\"]')?.click()")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // The drawer should be open
    let result = browser::shadow_eval(
        &page,
        "crap-drawer",
        "return root.querySelector('dialog')?.hasAttribute('open') ? 'true' : 'false';",
    )
    .await;
    assert_eq!(result, "true", "drawer should be open for filter builder");

    // Empty URL → empty drawer (post-fix). Click "Add condition" to
    // create a row.
    let _ = browser::shadow_eval(
        &page,
        "crap-drawer",
        "root.querySelector('.filter-builder > button.button--ghost')?.click(); return '';",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Now exactly one condition row should exist.
    let row_count = browser::shadow_eval(
        &page,
        "crap-drawer",
        "return String(root.querySelectorAll('.filter-builder__row').length);",
    )
    .await;
    let rows: i64 = row_count.parse().unwrap_or(0);
    assert_eq!(
        rows, 1,
        "after one click on Add condition the drawer should have 1 row, got {rows}"
    );

    let field_count = browser::shadow_eval(
        &page,
        "crap-drawer",
        "return String(root.querySelectorAll('.filter-builder__field').length);",
    )
    .await;
    let fields: i64 = field_count.parse().unwrap_or(0);
    assert!(
        fields > 0,
        "filter builder should have a field select, got {fields}"
    );

    server_handle.abort();
}

// ── filter_builder_preset_value_change_applies ────────────────────────────
//
// User's exact reproduction: open the filter drawer with a preset
// filter in the URL (status=published), switch the dropdown value
// to a different option (draft), click Apply, and confirm the new
// URL reflects the user's choice (where[status][equals]=draft) — not
// the original preset.

#[tokio::test(flavor = "multi_thread")]
async fn filter_builder_preset_value_change_applies() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_list_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "blist4@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "blist4@test.com");

    create_list_post(&app, "Filterable Post");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "blist4@test.com", "pass123").await;

    page.goto(format!(
        "{base_url}/admin/collections/posts?where[status][equals]=published"
    ))
    .await
    .unwrap()
    .wait_for_navigation()
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Open filter drawer.
    page.evaluate("() => document.querySelector('[data-action=\"open-filter-builder\"]')?.click()")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Sanity: the preset row hydrated to status/equals/published.
    let initial = browser::shadow_eval(
        &page,
        "crap-drawer",
        "return root.querySelector('[name=\"filter-value\"]')?.value || '';",
    )
    .await;
    assert_eq!(initial, "published");

    // User changes value dropdown to 'draft' (no op change).
    let _ = browser::shadow_eval(
        &page,
        "crap-drawer",
        "const sel = root.querySelector('[name=\"filter-value\"]'); \
         sel.value = 'draft'; \
         sel.dispatchEvent(new Event('change', { bubbles: true })); \
         return '';",
    )
    .await;

    // User clicks Apply (the primary button in the filter footer).
    let _ = browser::shadow_eval(
        &page,
        "crap-drawer",
        "root.querySelector('.filter-builder__footer .button--primary').click(); return '';",
    )
    .await;

    // Wait for `htmx.ajax(...)` navigation in `list-settings.js::navigate()`.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let url = page
        .evaluate("() => window.location.href")
        .await
        .unwrap()
        .into_value::<String>()
        .unwrap();

    assert!(
        url.contains("where%5Bstatus%5D%5Bequals%5D=draft")
            || url.contains("where[status][equals]=draft"),
        "URL must reflect the user's draft selection, got: {url}"
    );
    assert!(
        !url.contains("where%5Bstatus%5D%5Bequals%5D=published")
            && !url.contains("where[status][equals]=published"),
        "URL must NOT still contain the old published filter, got: {url}"
    );

    server_handle.abort();
}

// ── filter_builder_apply_actually_filters_the_list ────────────────────────
//
// User reproduction: with filter URL `?where[status][equals]=draft`,
// the rendered list still shows ALL items including published ones.
// This test creates a draft and a published post, lands on the filter
// URL via the browser flow (htmx.ajax navigation), and asserts that
// only the draft post appears in the rendered table.

#[tokio::test(flavor = "multi_thread")]
async fn filter_builder_apply_actually_filters_the_list() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_list_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "blistf@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "blistf@test.com");

    create_list_post_with_status(&app, "Draft Post Title", "draft");
    create_list_post_with_status(&app, "Published Post Title", "published");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "blistf@test.com", "pass123").await;

    // Hit list page unfiltered first — should show both.
    page.goto(format!("{base_url}/admin/collections/posts"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let unfiltered = page
        .evaluate("() => document.querySelectorAll('tbody tr').length")
        .await
        .unwrap()
        .into_value::<i64>()
        .unwrap();
    assert_eq!(unfiltered, 2, "unfiltered list should show 2 rows");

    // Open filter drawer, change value to draft, apply.
    page.evaluate("() => document.querySelector('[data-action=\"open-filter-builder\"]')?.click()")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Drawer opens empty (no URL filter). Click "+ Add condition" to
    // create a row, then configure it.
    let _ = browser::shadow_eval(
        &page,
        "crap-drawer",
        "root.querySelector('.filter-builder > button.button--ghost')?.click(); return '';",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    // The new row defaults to title field. Switch it to status.
    let _ = browser::shadow_eval(
        &page,
        "crap-drawer",
        "const fieldSel = root.querySelector('.filter-builder__field'); \
         fieldSel.value = 'status'; \
         fieldSel.dispatchEvent(new Event('change', { bubbles: true })); \
         return '';",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Now value-input should be a select with status options. Pick 'draft'.
    let _ = browser::shadow_eval(
        &page,
        "crap-drawer",
        "const sel = root.querySelector('[name=\"filter-value\"]'); \
         sel.value = 'draft'; \
         sel.dispatchEvent(new Event('change', { bubbles: true })); \
         return '';",
    )
    .await;

    // Click Apply.
    let _ = browser::shadow_eval(
        &page,
        "crap-drawer",
        "root.querySelector('.filter-builder__footer .button--primary').click(); return '';",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // After navigation: list must now show ONLY the draft post.
    let row_count = page
        .evaluate("() => document.querySelectorAll('tbody tr').length")
        .await
        .unwrap()
        .into_value::<i64>()
        .unwrap();
    let draft_visible = page
        .evaluate("() => document.body.textContent.includes('Draft Post Title')")
        .await
        .unwrap()
        .into_value::<bool>()
        .unwrap();
    let published_visible = page
        .evaluate("() => document.body.textContent.includes('Published Post Title')")
        .await
        .unwrap()
        .into_value::<bool>()
        .unwrap();

    assert_eq!(
        row_count, 1,
        "filtered list should narrow to 1 row, got {row_count}"
    );
    assert!(draft_visible, "draft post must be visible after filter");
    assert!(
        !published_visible,
        "published post must NOT be visible — filter is being silently dropped"
    );

    server_handle.abort();
}

// ── filter_builder_multi_row_reopen_edit_persists ─────────────────────────
//
// User scenario: land with one preset filter, add a second row,
// apply, re-open the drawer (now showing both filters), edit the
// second row's value, apply again. Both filters must end up in the
// new URL with the user's edited value.

#[tokio::test(flavor = "multi_thread")]
async fn filter_builder_multi_row_reopen_edit_persists() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_list_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "blist5@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "blist5@test.com");

    create_list_post(&app, "Filterable Post");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "blist5@test.com", "pass123").await;

    // Land on a URL that already has a filter; this becomes row 1.
    page.goto(format!(
        "{base_url}/admin/collections/posts?where[status][equals]=published"
    ))
    .await
    .unwrap()
    .wait_for_navigation()
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Open drawer, add a second filter row, apply.
    page.evaluate("() => document.querySelector('[data-action=\"open-filter-builder\"]')?.click()")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Click "Add condition" — the "+" button.
    let _ = browser::shadow_eval(
        &page,
        "crap-drawer",
        "const addBtn = root.querySelector('.filter-builder > button.button--ghost'); \
         addBtn.click(); return '';",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Now there should be two rows. Configure row 2: change its
    // field to "title" and put a value.
    let _ = browser::shadow_eval(
        &page,
        "crap-drawer",
        "const rows = root.querySelectorAll('.filter-builder__row'); \
         if (rows.length !== 2) throw new Error('expected 2 rows after add, got ' + rows.length); \
         const fieldSel = rows[1].querySelector('.filter-builder__field'); \
         fieldSel.value = 'title'; \
         fieldSel.dispatchEvent(new Event('change', { bubbles: true })); \
         const valInput = rows[1].querySelector('[name=\"filter-value\"]'); \
         valInput.value = 'foo'; \
         valInput.dispatchEvent(new Event('change', { bubbles: true })); \
         return '';",
    )
    .await;

    // Apply.
    let _ = browser::shadow_eval(
        &page,
        "crap-drawer",
        "root.querySelector('.filter-builder__footer .button--primary').click(); return '';",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let url1 = page
        .evaluate("() => window.location.href")
        .await
        .unwrap()
        .into_value::<String>()
        .unwrap();
    assert!(
        url1.contains("status%5D%5Bequals%5D=published")
            || url1.contains("status][equals]=published"),
        "first apply should keep status=published, got {url1}"
    );
    assert!(
        url1.contains("title%5D%5Bcontains%5D=foo")
            || url1.contains("title][contains]=foo")
            || url1.contains("title%5D%5Bequals%5D=foo")
            || url1.contains("title][equals]=foo"),
        "first apply should add title=foo, got {url1}"
    );

    // Re-open drawer. Both rows should be hydrated from URL.
    page.evaluate("() => document.querySelector('[data-action=\"open-filter-builder\"]')?.click()")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let row_count = browser::shadow_eval(
        &page,
        "crap-drawer",
        "return String(root.querySelectorAll('.filter-builder__row').length);",
    )
    .await;
    assert_eq!(
        row_count, "2",
        "drawer reopen should show both filters, got {row_count} rows"
    );

    // Change row 2's value from "foo" to "bar".
    let _ = browser::shadow_eval(
        &page,
        "crap-drawer",
        "const rows = root.querySelectorAll('.filter-builder__row'); \
         const valInput = rows[1].querySelector('[name=\"filter-value\"]'); \
         valInput.value = 'bar'; \
         valInput.dispatchEvent(new Event('change', { bubbles: true })); \
         return '';",
    )
    .await;

    // Apply.
    let _ = browser::shadow_eval(
        &page,
        "crap-drawer",
        "root.querySelector('.filter-builder__footer .button--primary').click(); return '';",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let url2 = page
        .evaluate("() => window.location.href")
        .await
        .unwrap()
        .into_value::<String>()
        .unwrap();
    assert!(
        url2.contains("status%5D%5Bequals%5D=published")
            || url2.contains("status][equals]=published"),
        "second apply must keep status=published, got {url2}"
    );
    assert!(
        url2.contains("=bar"),
        "second apply must reflect 'bar' edit, got {url2}"
    );
    assert!(
        !url2.contains("=foo"),
        "old 'foo' value must be replaced, got {url2}"
    );

    server_handle.abort();
}

// ── filter_builder_preserves_user_edit_across_op_change ───────────────────
//
// Regression: `_buildFilterRow` in `static/components/list-settings.js`
// captured the URL-derived `preset` by closure. When the user changed
// the op (which triggers `renderValue` to rebuild the value input
// because exists/not_exists drop the input), the rebuild used the
// stale `preset.value` and silently overwrote whatever the user had
// just selected. Symptom: open the filter drawer with a preset like
// `?where[status][equals]=published`, switch the value dropdown to
// "draft", then change the op — the value snaps back to "published".
//
// Fix: read the current DOM state (`valueWrap.querySelector('[name="filter-value"]')`)
// before the rebuild, fall back to `preset` only when the input
// doesn't yet exist or has no value.

#[tokio::test(flavor = "multi_thread")]
async fn filter_builder_preserves_user_edit_across_op_change() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_list_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "blist3@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "blist3@test.com");

    create_list_post(&app, "Filterable Post");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "blist3@test.com", "pass123").await;

    // Land with a preset filter in the URL: status equals published.
    page.goto(format!(
        "{base_url}/admin/collections/posts?where[status][equals]=published"
    ))
    .await
    .unwrap()
    .wait_for_navigation()
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Open filter drawer.
    page.evaluate("() => document.querySelector('[data-action=\"open-filter-builder\"]')?.click()")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Sanity-check: drawer rendered the preset row with status/equals/published.
    let preset_value = browser::shadow_eval(
        &page,
        "crap-drawer",
        "return root.querySelector('[name=\"filter-value\"]')?.value || '';",
    )
    .await;
    assert_eq!(
        preset_value, "published",
        "preset filter should hydrate the value dropdown to 'published'"
    );

    // User changes the value dropdown to 'draft'.
    let _ = browser::shadow_eval(
        &page,
        "crap-drawer",
        "const sel = root.querySelector('[name=\"filter-value\"]'); \
         sel.value = 'draft'; \
         sel.dispatchEvent(new Event('change', { bubbles: true })); \
         return '';",
    )
    .await;

    // User changes the op (equals → not_equals). This triggers the
    // value-input re-render that the bug used to clobber.
    let _ = browser::shadow_eval(
        &page,
        "crap-drawer",
        "const op = root.querySelector('.filter-builder__op'); \
         op.value = 'not_equals'; \
         op.dispatchEvent(new Event('change', { bubbles: true })); \
         return '';",
    )
    .await;

    // After the op change, the value input was rebuilt. The user's
    // 'draft' selection must survive — the bug returned 'published'.
    let after = browser::shadow_eval(
        &page,
        "crap-drawer",
        "return root.querySelector('[name=\"filter-value\"]')?.value || '';",
    )
    .await;
    assert_eq!(
        after, "draft",
        "user's value selection must survive an op change (was {after:?})"
    );

    server_handle.abort();
}

// ── filter_apply_strips_stale_cursor ──────────────────────────────────────
//
// Regression: `_buildFilterUrl` previously deleted only `where[…]`
// params on filter change, leaving `after_cursor` / `before_cursor`
// in the URL. The cursor was issued against the previous result set;
// with a different filter, the cursor's keyset comparison narrows
// the result to empty (or wrong-position rows). Fix: strip cursor
// params alongside `where[…]` and reset to `page=1`.

#[tokio::test(flavor = "multi_thread")]
async fn filter_apply_strips_stale_cursor() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_list_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "blistc@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "blistc@test.com");

    create_list_post(&app, "Cursor Land");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "blistc@test.com", "pass123").await;

    // Land on a URL that already has BOTH a where filter and a cursor —
    // no need to actually paginate; the JS only inspects URL params.
    page.goto(format!(
        "{base_url}/admin/collections/posts?where[title][equals]=foo&after_cursor=stale123"
    ))
    .await
    .unwrap()
    .wait_for_navigation()
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Open drawer, change the filter (title=foo → title=bar), apply.
    page.evaluate("() => document.querySelector('[data-action=\"open-filter-builder\"]')?.click()")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let _ = browser::shadow_eval(
        &page,
        "crap-drawer",
        "const valInput = root.querySelector('[name=\"filter-value\"]'); \
         valInput.value = 'bar'; \
         valInput.dispatchEvent(new Event('change', { bubbles: true })); \
         root.querySelector('.filter-builder__footer .button--primary').click(); \
         return '';",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let url = page
        .evaluate("() => window.location.href")
        .await
        .unwrap()
        .into_value::<String>()
        .unwrap();
    assert!(
        !url.contains("after_cursor"),
        "stale cursor must be stripped from URL on filter apply, got: {url}"
    );
    assert!(
        url.contains("title%5D%5Bequals%5D=bar") || url.contains("title][equals]=bar"),
        "new filter must be applied, got: {url}"
    );

    server_handle.abort();
}

// ── filter_drawer_empty_when_no_url_filters ───────────────────────────────
//
// Regression: the filter drawer used to auto-build a single empty row
// hydrated to the first field's first op + first value (typically
// `_status = "published"` for collections with drafts). Clicking
// Apply without configuring silently wrote
// `?where[_status][equals]=published` and narrowed the list. Now the
// drawer opens with zero rows when the URL has no filters.

#[tokio::test(flavor = "multi_thread")]
async fn filter_drawer_empty_when_no_url_filters() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_list_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "bliste@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "bliste@test.com");

    create_list_post(&app, "Will Stay Visible");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "bliste@test.com", "pass123").await;

    // Land unfiltered.
    page.goto(format!("{base_url}/admin/collections/posts"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    page.evaluate("() => document.querySelector('[data-action=\"open-filter-builder\"]')?.click()")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Empty URL → empty drawer. Zero rows, just the "+ Add condition"
    // button + footer.
    let row_count = browser::shadow_eval(
        &page,
        "crap-drawer",
        "return String(root.querySelectorAll('.filter-builder__row').length);",
    )
    .await;
    assert_eq!(
        row_count, "0",
        "drawer should open empty when no URL filters are present, got {row_count} rows"
    );

    // Click Apply without configuring anything. URL must NOT gain a
    // where[…] entry — previously it would default-hydrate to
    // `_status=published` and silently filter.
    let _ = browser::shadow_eval(
        &page,
        "crap-drawer",
        "root.querySelector('.filter-builder__footer .button--primary').click(); return '';",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let url = page
        .evaluate("() => window.location.href")
        .await
        .unwrap()
        .into_value::<String>()
        .unwrap();
    assert!(
        !url.contains("where%5B") && !url.contains("where["),
        "empty drawer + apply must NOT inject any filter, got: {url}"
    );

    server_handle.abort();
}

// ── filter_drawer_status_field_narrows_and_clears ─────────────────────────
//
// Regression for `_status` filter routing through the drawer. With
// drafts enabled the filter drawer exposes `_status` as a select field
// with values `[All, Published, Draft]`. Picking `draft` and applying
// pushes `?where[_status][equals]=draft` and narrows the list. Switching
// the value to "All" (empty) and applying drops the filter on the URL
// — `_collectFilters` skips empty-value rows — and the list shows every
// row again.

#[tokio::test(flavor = "multi_thread")]
async fn filter_drawer_status_field_narrows_and_clears() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_list_def_with_drafts(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "blistt@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "blistt@test.com");

    create_post_with_system_status(&app, "Pending Draft", "draft");
    create_post_with_system_status(&app, "Live Article", "published");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "blistt@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/posts"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let initial = page
        .evaluate("() => document.querySelectorAll('tbody tr').length")
        .await
        .unwrap()
        .into_value::<i64>()
        .unwrap();
    assert_eq!(
        initial, 2,
        "unfiltered list should show both draft and published"
    );

    // Open drawer, add row, set field to _status, value to draft, apply.
    page.evaluate("() => document.querySelector('[data-action=\"open-filter-builder\"]')?.click()")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let _ = browser::shadow_eval(
        &page,
        "crap-drawer",
        "root.querySelector('.filter-builder > button.button--ghost')?.click(); return '';",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let _ = browser::shadow_eval(
        &page,
        "crap-drawer",
        "const fieldSel = root.querySelector('.filter-builder__field'); \
         fieldSel.value = '_status'; \
         fieldSel.dispatchEvent(new Event('change', { bubbles: true })); \
         return '';",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let _ = browser::shadow_eval(
        &page,
        "crap-drawer",
        "const sel = root.querySelector('[name=\"filter-value\"]'); \
         sel.value = 'draft'; \
         sel.dispatchEvent(new Event('change', { bubbles: true })); \
         return '';",
    )
    .await;

    let _ = browser::shadow_eval(
        &page,
        "crap-drawer",
        "root.querySelector('.filter-builder__footer .button--primary').click(); return '';",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let url = page
        .evaluate("() => window.location.href")
        .await
        .unwrap()
        .into_value::<String>()
        .unwrap();
    assert!(
        url.contains("_status%5D%5Bequals%5D=draft") || url.contains("_status][equals]=draft"),
        "URL must reflect _status=draft, got: {url}"
    );
    let drafts_only = page
        .evaluate("() => document.querySelectorAll('tbody tr').length")
        .await
        .unwrap()
        .into_value::<i64>()
        .unwrap();
    assert_eq!(drafts_only, 1, "_status=draft must narrow to 1 row");
    let body_after = page
        .evaluate("() => document.body.innerText")
        .await
        .unwrap()
        .into_value::<String>()
        .unwrap();
    assert!(body_after.contains("Pending Draft"));
    assert!(!body_after.contains("Live Article"));

    // Re-open drawer; the row hydrates back to draft. Switch the value
    // to "All" (empty) and apply — the filter must drop off the URL.
    page.evaluate("() => document.querySelector('[data-action=\"open-filter-builder\"]')?.click()")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let _ = browser::shadow_eval(
        &page,
        "crap-drawer",
        "const sel = root.querySelector('[name=\"filter-value\"]'); \
         sel.value = ''; \
         sel.dispatchEvent(new Event('change', { bubbles: true })); \
         return '';",
    )
    .await;

    let _ = browser::shadow_eval(
        &page,
        "crap-drawer",
        "root.querySelector('.filter-builder__footer .button--primary').click(); return '';",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let url2 = page
        .evaluate("() => window.location.href")
        .await
        .unwrap()
        .into_value::<String>()
        .unwrap();
    assert!(
        !url2.contains("_status%5D%5Bequals%5D=draft")
            && !url2.contains("_status][equals]=draft")
            && !url2.contains("_status%5D%5Bequals%5D=published")
            && !url2.contains("_status][equals]=published"),
        "URL must drop the _status filter when value is 'All', got: {url2}"
    );
    let all_again = page
        .evaluate("() => document.querySelectorAll('tbody tr').length")
        .await
        .unwrap()
        .into_value::<i64>()
        .unwrap();
    assert_eq!(
        all_again, 2,
        "All (empty _status) must show both rows again, got {all_again}"
    );

    server_handle.abort();
}

// ── filter_drawer_status_field_hidden_without_drafts ──────────────────────
//
// Collections with `versions = None` (or `drafts = false`) have no
// `_status` column to filter on; the field is excluded from the filter
// drawer's field-select so users aren't offered a non-functional
// filter.

#[tokio::test(flavor = "multi_thread")]
async fn filter_drawer_status_field_hidden_without_drafts() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_list_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "blistnd@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "blistnd@test.com");

    create_list_post(&app, "Sample");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "blistnd@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/posts"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    page.evaluate("() => document.querySelector('[data-action=\"open-filter-builder\"]')?.click()")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let _ = browser::shadow_eval(
        &page,
        "crap-drawer",
        "root.querySelector('.filter-builder > button.button--ghost')?.click(); return '';",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let has_status_option = browser::shadow_eval(
        &page,
        "crap-drawer",
        "const fieldSel = root.querySelector('.filter-builder__field'); \
         return fieldSel ? Array.from(fieldSel.options).some(o => o.value === '_status').toString() : 'false';",
    )
    .await;
    assert_eq!(
        has_status_option, "false",
        "_status field must be absent from filter drawer when collection has no drafts"
    );

    server_handle.abort();
}
