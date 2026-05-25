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
    db::query,
};

use crap_cms_e2e::{browser, helpers::*};

fn make_confirm_def() -> CollectionDefinition {
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
    ];
    def
}

/// Create a post document and return its ID.
fn create_post(app: &TestApp, title: &str) -> String {
    let def = app.registry.get_collection("posts").unwrap().clone();

    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields = HashMap::from([("title".to_string(), json!(title))]).into();
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();
    doc.id.to_string()
}

// ── delete_shows_confirm_dialog ──────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn delete_shows_confirm_dialog() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_confirm_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "bconf1@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "bconf1@test.com");
    let doc_id = create_post(&app, "Delete Me");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "bconf1@test.com", "pass123").await;

    // Navigate to the delete page
    page.goto(format!(
        "{base_url}/admin/collections/posts/{doc_id}/delete"
    ))
    .await
    .unwrap()
    .wait_for_navigation()
    .await
    .unwrap();
    // Wait for JS components (especially <crap-confirm>) to register
    sleep(Duration::from_millis(500)).await;

    // Click the delete button (the form is wrapped by <crap-confirm>; the
    // confirm prompt is rendered by the page-singleton <crap-confirm-dialog>).
    let click_result = page
        .evaluate(
            "() => { \
            const btn = document.querySelector('button.button--danger'); \
            if (!btn) return 'no-button'; \
            btn.click(); \
            const dlgHost = document.querySelector('crap-confirm-dialog'); \
            if (!dlgHost?.shadowRoot) return 'no-shadow'; \
            const dialog = dlgHost.shadowRoot.querySelector('dialog'); \
            return dialog && dialog.hasAttribute('open') ? 'open' : 'closed'; \
        }",
        )
        .await
        .unwrap();
    let status: String = click_result.into_value().unwrap();
    assert_eq!(
        status, "open",
        "confirm dialog should be open after clicking delete, got: {status}"
    );

    server_handle.abort();
}

// ── confirm_cancel_stays_on_page ─────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn confirm_cancel_stays_on_page() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_confirm_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "bconf2@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "bconf2@test.com");
    let doc_id = create_post(&app, "Keep Me");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "bconf2@test.com", "pass123").await;

    page.goto(format!(
        "{base_url}/admin/collections/posts/{doc_id}/delete"
    ))
    .await
    .unwrap()
    .wait_for_navigation()
    .await
    .unwrap();
    // Wait for JS components to register
    sleep(Duration::from_millis(500)).await;

    // Click delete to open confirm dialog
    page.find_element("button.button--danger")
        .await
        .unwrap()
        .click()
        .await
        .unwrap();
    sleep(Duration::from_millis(300)).await;

    // Click cancel in the confirm-dialog shadow DOM
    page.evaluate(
        "() => document.querySelector('crap-confirm-dialog').shadowRoot.querySelector('.cancel').click()",
    )
    .await
    .unwrap();
    sleep(Duration::from_millis(300)).await;

    // Should still be on the delete page
    let url = page.url().await.unwrap().unwrap_or_default();
    assert!(
        url.contains("/delete"),
        "should stay on delete page after canceling, got: {url}"
    );

    // Dialog should be closed
    let is_open = browser::shadow_eval(
        &page,
        "crap-confirm-dialog",
        "return root.querySelector('dialog')?.hasAttribute('open') ? 'true' : 'false';",
    )
    .await;
    assert_eq!(is_open, "false", "dialog should be closed after cancel");

    server_handle.abort();
}

// ── confirm_accept_deletes ───────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn confirm_accept_deletes() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_confirm_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "bconf3@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "bconf3@test.com");
    let doc_id = create_post(&app, "Goodbye Post");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "bconf3@test.com", "pass123").await;

    page.goto(format!(
        "{base_url}/admin/collections/posts/{doc_id}/delete"
    ))
    .await
    .unwrap()
    .wait_for_navigation()
    .await
    .unwrap();
    // Wait for JS components to register
    sleep(Duration::from_millis(500)).await;

    // Click delete to trigger confirm
    page.find_element("button.button--danger")
        .await
        .unwrap()
        .click()
        .await
        .unwrap();
    sleep(Duration::from_millis(300)).await;

    // Click confirm in the confirm-dialog shadow DOM
    page.evaluate(
        "() => document.querySelector('crap-confirm-dialog').shadowRoot.querySelector('.confirm').click()",
    )
    .await
    .unwrap();
    sleep(Duration::from_secs(1)).await;

    // Should redirect away from the delete page (to the list)
    let url = page.url().await.unwrap().unwrap_or_default();
    assert!(
        !url.contains("/delete"),
        "should navigate away from delete page after confirming, got: {url}"
    );

    server_handle.abort();
}
