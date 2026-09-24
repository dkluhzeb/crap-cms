//! `<crap-relationship-search>` behaviour: inside array rows the hidden input
//! it submits must follow its row through duplicate / remove / reorder /
//! nested add; the has-one "View" link of a polymorphic pick must point at the
//! picked document; a refused search reads as an error; and the inline-create
//! panel toasts a refused save once.
#![allow(clippy::missing_panics_doc, clippy::too_many_lines)]

use std::time::Duration;

use chromiumoxide::Page;
use tokio::time::sleep;

use crap_cms::core::{collection::*, field::*};

use crap_cms_e2e::{BrowserTestCtx, browser, helpers::*, setup_browser_test};

fn make_categories_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("categories");
    def.timestamps = true;
    def.fields = vec![FieldDefinition::builder("name", FieldType::Text).build()];
    def
}

fn make_tags_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("tags");
    def.timestamps = true;
    def.fields = vec![FieldDefinition::builder("name", FieldType::Text).build()];
    def
}

fn author_field() -> FieldDefinition {
    FieldDefinition::builder("author", FieldType::Relationship)
        .relationship(RelationshipConfig::new("categories", false))
        .build()
}

/// `teams.members[]` with a has-one relationship per row.
fn make_team_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("teams");
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("name", FieldType::Text).build(),
        FieldDefinition::builder("members", FieldType::Array)
            .fields(vec![author_field()])
            .build(),
    ];
    def
}

/// `books.sections[].items[]` — the relationship lives two arrays deep.
fn make_book_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("books");
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("name", FieldType::Text).build(),
        FieldDefinition::builder("sections", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("items", FieldType::Array)
                    .fields(vec![author_field()])
                    .build(),
            ])
            .build(),
    ];
    def
}

/// Polymorphic has-one on a plain (non-array) field.
fn make_link_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("links");
    def.timestamps = true;
    let mut rel = RelationshipConfig::new("categories", false);
    rel.polymorphic = vec!["categories".into(), "tags".into()];
    def.fields = vec![
        FieldDefinition::builder("name", FieldType::Text).build(),
        FieldDefinition::builder("target", FieldType::Relationship)
            .relationship(rel)
            .build(),
    ];
    def
}

async fn open_create(page: &Page, base_url: &str, slug: &str) {
    page.goto(format!("{base_url}/admin/collections/{slug}/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    browser::wait_for_js(page, "customElements.get('crap-array-field')").await;
    browser::wait_for_js(page, "customElements.get('crap-relationship-search')").await;
}

/// Click the top-level field's own "add row" button.
async fn add_outer_row(page: &Page, expected_rows: usize) {
    page.evaluate(
        "() => { \
            const outer = document.querySelector('crap-array-field'); \
            const btn = [...outer.querySelectorAll('button[data-action=\"add-array-row\"]')] \
                .find((b) => b.closest('crap-array-field') === outer); \
            btn.click(); \
        }",
    )
    .await
    .unwrap();

    browser::wait_for_js(
        page,
        &format!(
            "document.querySelector('crap-array-field .form__array-rows').children.length === {expected_rows}"
        ),
    )
    .await;
}

/// Click the row-level action `action` on the top-level row `row`.
async fn row_action(page: &Page, row: usize, action: &str) {
    page.evaluate(format!(
        "() => {{ \
            const rows = document.querySelector('crap-array-field .form__array-rows').children; \
            const row = rows[{row}]; \
            const btn = [...row.querySelectorAll('[data-action=\"{action}\"]')] \
                .find((b) => b.closest('.form__array-row') === row); \
            btn.click(); \
        }}"
    ))
    .await
    .unwrap();
}

/// Pick `id` in the `nth` relationship widget of the page (document order).
async fn pick(page: &Page, nth: usize, id: &str) {
    page.evaluate(format!(
        "() => document.querySelectorAll('crap-relationship-search')[{nth}] \
            .dispatchEvent(new CustomEvent('crap:pick', {{ detail: {{ id: '{id}', label: '{id}' }} }}))"
    ))
    .await
    .unwrap();
}

/// Every relationship hidden input as `name=value`, in document order.
async fn hidden_inputs(page: &Page) -> Vec<String> {
    page.evaluate(
        "() => [...document.querySelectorAll('crap-relationship-search input[type=\"hidden\"]')] \
            .map((i) => `${i.name}=${i.value}`)",
    )
    .await
    .unwrap()
    .into_value()
    .unwrap()
}

async fn setup(defs: Vec<CollectionDefinition>, email: &str) -> BrowserTestCtx {
    setup_browser_test(defs, vec![], email, "pass123").await
}

// ── duplicate then change the copy ─────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn duplicated_row_pick_writes_to_the_copy() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        app: _app,
        ..
    } = setup(
        vec![make_categories_def(), make_team_def(), make_users_def()],
        "rsdup@test.com",
    )
    .await;
    open_create(&page, &base_url, "teams").await;

    add_outer_row(&page, 1).await;
    pick(&page, 0, "a").await;

    row_action(&page, 0, "duplicate-row").await;
    browser::wait_for_element_count(&page, "crap-relationship-search", 2).await;
    pick(&page, 1, "b").await;

    assert_eq!(
        hidden_inputs(&page).await,
        vec!["members[0][author]=a", "members[1][author]=b"],
        "the copy's pick is submitted under the copy's own row index"
    );

    server_handle.abort();
}

// ── remove a row, then change a later row ──────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn pick_after_removing_an_earlier_row_uses_the_new_index() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        app: _app,
        ..
    } = setup(
        vec![make_categories_def(), make_team_def(), make_users_def()],
        "rsrem@test.com",
    )
    .await;
    open_create(&page, &base_url, "teams").await;

    add_outer_row(&page, 1).await;
    add_outer_row(&page, 2).await;

    row_action(&page, 0, "remove-array-row").await;
    browser::wait_for_element_count(&page, "crap-relationship-search", 1).await;
    pick(&page, 0, "b").await;

    assert_eq!(
        hidden_inputs(&page).await,
        vec!["members[0][author]=b"],
        "the surviving row was re-numbered to 0; its pick must follow"
    );

    server_handle.abort();
}

// ── reorder, then change ────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn pick_after_moving_a_row_uses_the_new_index() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        app: _app,
        ..
    } = setup(
        vec![make_categories_def(), make_team_def(), make_users_def()],
        "rsmove@test.com",
    )
    .await;
    open_create(&page, &base_url, "teams").await;

    add_outer_row(&page, 1).await;
    add_outer_row(&page, 2).await;
    pick(&page, 0, "a").await;
    pick(&page, 1, "b").await;

    // Row "b" moves to the top, then its pick changes.
    row_action(&page, 1, "move-row-up").await;
    browser::wait_for_js(
        &page,
        "document.querySelector('crap-relationship-search input[type=\"hidden\"]').value === 'b'",
    )
    .await;
    pick(&page, 0, "c").await;

    assert_eq!(
        hidden_inputs(&page).await,
        vec!["members[0][author]=c", "members[1][author]=a"],
    );

    server_handle.abort();
}

// ── nested add inside a later outer row ─────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn nested_row_pick_is_named_after_its_outer_row() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        app: _app,
        ..
    } = setup(
        vec![make_categories_def(), make_book_def(), make_users_def()],
        "rsnest@test.com",
    )
    .await;
    open_create(&page, &base_url, "books").await;

    add_outer_row(&page, 1).await;
    add_outer_row(&page, 2).await;

    // Add an inner row inside the SECOND outer row.
    page.evaluate(
        "() => { \
            const outer = document.querySelector('crap-array-field .form__array-rows').children[1]; \
            const inner = outer.querySelector('crap-array-field'); \
            const btn = [...inner.querySelectorAll('button[data-action=\"add-array-row\"]')] \
                .find((b) => b.closest('crap-array-field') === inner); \
            btn.click(); \
        }",
    )
    .await
    .unwrap();
    browser::wait_for_element_count(&page, "crap-relationship-search", 1).await;
    pick(&page, 0, "a").await;

    assert_eq!(
        hidden_inputs(&page).await,
        vec!["sections[1][items][0][author]=a"],
        "the inner row keeps its outer row's index"
    );

    server_handle.abort();
}

// ── polymorphic has-one "View" link ────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn polymorphic_view_link_points_at_the_picked_document() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        app: _app,
        ..
    } = setup(
        vec![
            make_categories_def(),
            make_tags_def(),
            make_link_def(),
            make_users_def(),
        ],
        "rspoly@test.com",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/links/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    browser::wait_for_js(&page, "customElements.get('crap-relationship-search')").await;

    page.evaluate(
        "() => document.querySelector('crap-relationship-search') \
            .dispatchEvent(new CustomEvent('crap:pick', { detail: { id: 'tags/t1', label: 'T1', collection: 'tags' } }))",
    )
    .await
    .unwrap();

    let href: String = page
        .evaluate(
            "() => document.querySelector('.relationship-field__view-link').getAttribute('href')",
        )
        .await
        .unwrap()
        .into_value()
        .unwrap();
    assert_eq!(href, "/admin/collections/tags/t1");

    server_handle.abort();
}

// ── a refused search is an error, not "no results" ─────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_search_shows_an_error_not_no_results() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        app: _app,
        ..
    } = setup(
        vec![
            make_categories_def(),
            make_tags_def(),
            make_link_def(),
            make_users_def(),
        ],
        "rssearch@test.com",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/links/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    browser::wait_for_element(&page, ".relationship-search__input").await;

    // Every search answers 500.
    page.evaluate(
        "() => { \
            const real = window.fetch; \
            window.fetch = (url, opts) => String(url).includes('/admin/api/search/') \
                ? Promise.resolve(new Response('boom', { status: 500 })) \
                : real(url, opts); \
            document.querySelector('.relationship-search__input').focus(); \
        }",
    )
    .await
    .unwrap();

    assert!(
        browser::wait_for_element(&page, ".relationship-search__error").await,
        "a failed search must say so"
    );
    let no_results = page
        .evaluate(
            "() => document.querySelectorAll('.relationship-search__empty:not(.relationship-search__error)').length",
        )
        .await
        .unwrap()
        .into_value::<i64>()
        .unwrap();
    assert_eq!(no_results, 0, "not reported as an empty result list");

    server_handle.abort();
}

// ── the inline-create panel does not toast twice ────────────────────────────

/// Regression: the panel toasted a response's `X-Crap-Toast` itself, and the
/// page-level toast host showed the same header again as the event bubbled.
#[tokio::test(flavor = "multi_thread")]
async fn inline_create_panel_error_is_toasted_once() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        app: _app,
        ..
    } = setup(
        vec![
            make_categories_def(),
            make_tags_def(),
            make_link_def(),
            make_users_def(),
        ],
        "rspanel@test.com",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/links/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    browser::wait_for_element(&page, "[data-inline-create=\"categories\"]").await;

    page.evaluate("() => document.querySelector('[data-inline-create=\"categories\"]').click()")
        .await
        .unwrap();
    browser::wait_for_element(&page, ".create-panel__body [data-create-panel-form]").await;

    // A refused save's response, as htmx reports it.
    page.evaluate(
        "() => { \
            const form = document.querySelector('.create-panel__body [data-create-panel-form]'); \
            const header = JSON.stringify({ message: 'Refused', type: 'error' }); \
            form.dispatchEvent(new CustomEvent('htmx:afterRequest', { bubbles: true, detail: { \
                successful: false, \
                xhr: { status: 422, getResponseHeader: (n) => (n === 'X-Crap-Toast' ? header : null) }, \
            } })); \
        }",
    )
    .await
    .unwrap();

    browser::wait_for_js(
        &page,
        "document.querySelector('crap-toast').shadowRoot.querySelector('.toast')",
    )
    .await;
    // Both listeners run synchronously on the one event; give a stray second
    // toast a moment to land before counting.
    sleep(Duration::from_millis(200)).await;

    let count: i64 = page
        .evaluate(
            "() => [...document.querySelector('crap-toast').shadowRoot.querySelectorAll('.toast')] \
                .filter((t) => t.textContent === 'Refused').length",
        )
        .await
        .unwrap()
        .into_value()
        .unwrap();
    assert_eq!(count, 1, "one toast per response");

    server_handle.abort();
}
