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

use std::collections::HashMap;
use std::time::Duration;

use serde_json::json;
use tokio::time::sleep;

use crap_cms::{
    core::{DocumentFields, collection::*, field::*},
    db::query,
};
use crap_cms_e2e::{browser, helpers::*};

fn make_filter_def() -> CollectionDefinition {
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
        FieldDefinition::builder("status", FieldType::Text).build(),
    ];
    def
}

fn seed_post(app: &TestApp, title: &str, status: &str) {
    let def = app.registry.get_collection("posts").unwrap().clone();
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields = HashMap::from([
        ("title".to_string(), json!(title)),
        ("status".to_string(), json!(status)),
    ])
    .into();
    query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();
}

// ── filter_builder_adds_multiple_conditions ──────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn filter_builder_adds_multiple_conditions() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_filter_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "bfilta1@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "bfilta1@test.com");

    seed_post(&app, "First", "draft");
    seed_post(&app, "Second", "published");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "bfilta1@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/posts"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Wait for the filter-builder trigger to render AND for its owning component
    // (`crap-list-settings`) to upgrade — the button exists in server HTML, but a
    // click before the component wires its delegated handler is a no-op under
    // load (the drawer never opens, so the filter-builder never renders).
    browser::wait_for_element(&page, "[data-action=\"open-filter-builder\"]").await;
    browser::wait_for_js(&page, "customElements.get('crap-list-settings')").await;

    page.evaluate("() => document.querySelector('[data-action=\"open-filter-builder\"]')?.click()")
        .await
        .unwrap();

    // Wait for the drawer's filter-builder (shadow DOM) to render its Add button.
    for _ in 0..60 {
        let ready = browser::shadow_eval(
            &page,
            "crap-drawer",
            "return root.querySelector('.filter-builder > button.button--ghost') ? 'true' : 'false';",
        )
        .await;
        if ready == "true" {
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }

    // Add three conditions in sequence by clicking "Add" three times, polling
    // until each new row appears before clicking again (shadow DOM, no light-DOM
    // signal available so we poll shadow_eval).
    for i in 0..3 {
        let _ = browser::shadow_eval(
            &page,
            "crap-drawer",
            "root.querySelector('.filter-builder > button.button--ghost')?.click(); return '';",
        )
        .await;

        let expected = i + 1;
        for _ in 0..60 {
            let count = browser::shadow_eval(
                &page,
                "crap-drawer",
                "return String(root.querySelectorAll('.filter-builder__row').length);",
            )
            .await;
            if count.parse::<i64>().unwrap_or(0) >= expected {
                break;
            }
            sleep(Duration::from_millis(50)).await;
        }
    }

    let row_count = browser::shadow_eval(
        &page,
        "crap-drawer",
        "return String(root.querySelectorAll('.filter-builder__row').length);",
    )
    .await;
    let rows: i64 = row_count.parse().unwrap_or(0);
    assert_eq!(rows, 3, "should have 3 condition rows, got {rows}");

    server_handle.abort();
}

// ── filter_builder_removes_condition ─────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn filter_builder_removes_condition() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_filter_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "bfilta2@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "bfilta2@test.com");

    seed_post(&app, "Only", "draft");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "bfilta2@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/posts"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Wait for the filter-builder trigger to render AND for its owning component
    // (`crap-list-settings`) to upgrade — the button exists in server HTML, but a
    // click before the component wires its delegated handler is a no-op under
    // load (the drawer never opens, so the filter-builder never renders).
    browser::wait_for_element(&page, "[data-action=\"open-filter-builder\"]").await;
    browser::wait_for_js(&page, "customElements.get('crap-list-settings')").await;

    page.evaluate("() => document.querySelector('[data-action=\"open-filter-builder\"]')?.click()")
        .await
        .unwrap();

    // Wait for the drawer's filter-builder (shadow DOM) to render its Add button.
    for _ in 0..60 {
        let ready = browser::shadow_eval(
            &page,
            "crap-drawer",
            "return root.querySelector('.filter-builder > button.button--ghost') ? 'true' : 'false';",
        )
        .await;
        if ready == "true" {
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }

    // Add two conditions, polling until each row appears before the next click.
    for i in 0..2 {
        let _ = browser::shadow_eval(
            &page,
            "crap-drawer",
            "root.querySelector('.filter-builder > button.button--ghost')?.click(); return '';",
        )
        .await;

        let expected = i + 1;
        for _ in 0..60 {
            let count = browser::shadow_eval(
                &page,
                "crap-drawer",
                "return String(root.querySelectorAll('.filter-builder__row').length);",
            )
            .await;
            if count.parse::<i64>().unwrap_or(0) >= expected {
                break;
            }
            sleep(Duration::from_millis(50)).await;
        }
    }

    let before = browser::shadow_eval(
        &page,
        "crap-drawer",
        "return String(root.querySelectorAll('.filter-builder__row').length);",
    )
    .await;
    assert_eq!(before, "2", "should have 2 rows before removal");

    // Click the remove button on the first row. The row's trailing icon
    // button is the remove control.
    let _ = browser::shadow_eval(
        &page,
        "crap-drawer",
        "const row = root.querySelector('.filter-builder__row'); \
         const btn = row?.querySelector('button.filter-builder__remove'); \
         btn?.click(); return '';",
    )
    .await;

    // Poll (shadow DOM) until the row count drops to 1 after removal.
    for _ in 0..60 {
        let count = browser::shadow_eval(
            &page,
            "crap-drawer",
            "return String(root.querySelectorAll('.filter-builder__row').length);",
        )
        .await;
        if count.parse::<i64>().unwrap_or(-1) == 1 {
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }

    let after = browser::shadow_eval(
        &page,
        "crap-drawer",
        "return String(root.querySelectorAll('.filter-builder__row').length);",
    )
    .await;
    let rows: i64 = after.parse().unwrap_or(-1);
    assert_eq!(rows, 1, "should have 1 row after removing one, got {after}");

    server_handle.abort();
}
