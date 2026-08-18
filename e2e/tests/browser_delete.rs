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

use serde_json::json;

use crap_cms::core::DocumentFields;
use crap_cms::core::collection::*;
use crap_cms::core::field::*;
use crap_cms::db::query;
use crap_cms_e2e::{BrowserTestCtx, browser, helpers::*, setup_browser_test};

// `<crap-delete-dialog>` is rendered as a singleton in `templates/layout/base.hbs`
// and binds to any `[data-delete-id]` trigger via event delegation
// (`static/components/delete-dialog.js`). It appears on list rows
// (`templates/collections/items_row.hbs`) and the edit sidebar
// (`templates/collections/edit_sidebar.hbs`).
//
// These tests drive the dialog from the LIST page rather than the edit
// page. The edit page (`/admin/collections/{slug}/{id}`) currently keeps
// chromiumoxide's `page.goto()` blocked on `loadEventFired` indefinitely
// — likely because the page holds a background fetch open (autosave,
// SSE, etc.) that the load-event tracking treats as in-flight. Both
// surfaces share the same dialog component, so list-page coverage
// exercises the same JS path.

// ── delete_button_click_opens_dialog ─────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn delete_button_click_opens_dialog() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        app,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_soft_posts_def(), make_users_def()],
        vec![],
        "bdelete1@test.com",
        "pass123",
    )
    .await;

    seed_post(&app, "Doomed Post");

    page.goto(format!("{base_url}/admin/collections/posts"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Trigger should be present on the list row.
    let has_button = page
        .evaluate("() => !!document.querySelector('button[data-delete-id]')")
        .await
        .unwrap()
        .into_value::<bool>()
        .unwrap_or(false);
    assert!(has_button, "delete button should exist on list row");

    page.evaluate("() => document.querySelector('button[data-delete-id]')?.click()")
        .await
        .unwrap();

    // Poll until the shadow-DOM dialog gains the open attribute
    assert!(
        browser::wait_for_js(
            &page,
            "!!document.querySelector('crap-delete-dialog')?.shadowRoot\
                ?.querySelector('dialog')?.hasAttribute('open')"
        )
        .await,
        "delete dialog should be open after clicking trigger"
    );

    let is_open = page
        .evaluate(
            r"() => {
                const host = document.querySelector('crap-delete-dialog');
                if (!host || !host.shadowRoot) return false;
                const dlg = host.shadowRoot.querySelector('dialog');
                return !!dlg && dlg.hasAttribute('open');
            }",
        )
        .await
        .unwrap()
        .into_value::<bool>()
        .unwrap_or(false);
    assert!(
        is_open,
        "delete dialog should be open after clicking trigger"
    );

    server_handle.abort();
}

// ── delete_dialog_shows_doc_title ────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn delete_dialog_shows_doc_title() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        app,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_soft_posts_def(), make_users_def()],
        vec![],
        "bdelete2@test.com",
        "pass123",
    )
    .await;

    seed_post(&app, "Unique Title For Test");

    page.goto(format!("{base_url}/admin/collections/posts"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    page.evaluate("() => document.querySelector('button[data-delete-id]')?.click()")
        .await
        .unwrap();

    // Poll until the dialog's shadow content includes the doc title
    assert!(
        browser::wait_for_js(
            &page,
            "(document.querySelector('crap-delete-dialog')?.shadowRoot?.textContent ?? '')\
                .includes('Unique Title For Test')"
        )
        .await,
        "dialog should display the doc title"
    );

    let text = page
        .evaluate(
            r"() => {
                const host = document.querySelector('crap-delete-dialog');
                if (!host || !host.shadowRoot) return '';
                return host.shadowRoot.textContent || '';
            }",
        )
        .await
        .unwrap()
        .into_value::<String>()
        .unwrap_or_default();
    assert!(
        text.contains("Unique Title For Test"),
        "dialog should display the doc title, got: {text:?}"
    );

    server_handle.abort();
}

fn make_soft_posts_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def.timestamps = true;
    def.soft_delete = true;
    def.admin = AdminConfig {
        use_as_title: Some("title".to_string()),
        ..AdminConfig::default()
    };
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
    ];
    def
}

fn seed_post(app: &TestApp, title: &str) -> String {
    let def = app.registry.get_collection("posts").unwrap().clone();
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields = HashMap::from([("title".to_string(), json!(title))]).into();
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();
    doc.id.to_string()
}
