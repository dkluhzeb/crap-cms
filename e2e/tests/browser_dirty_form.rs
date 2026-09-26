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

use crap_cms::{
    core::{DocumentFields, collection::*, field::*},
    db::query,
};

use crap_cms_e2e::{BrowserTestCtx, browser, helpers::*, setup_browser_test};

fn make_dirty_def() -> CollectionDefinition {
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

// ── dirty_form_not_armed_on_clean ────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn dirty_form_not_armed_on_clean() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_dirty_def(), make_users_def()],
        vec![],
        "bdirty1@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/posts/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Wait for the component to arm itself (requestAnimationFrame)
    assert!(
        browser::wait_for_js(
            &page,
            "document.querySelector('crap-dirty-form')?._armed === true"
        )
        .await,
        "dirty form should arm itself after load"
    );

    // Without any interaction, _dirty should be false
    let result = page
        .evaluate("() => { const df = document.querySelector('crap-dirty-form'); return df ? df._dirty : null; }")
        .await
        .unwrap();
    let dirty: bool = result.into_value().unwrap_or(false);
    assert!(!dirty, "dirty form should not be dirty on a clean page");

    server_handle.abort();
}

// ── dirty_form_armed_after_input ─────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn dirty_form_armed_after_input() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_dirty_def(), make_users_def()],
        vec![],
        "bdirty2@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/posts/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Wait for arming before interacting (an input before arming is ignored)
    assert!(
        browser::wait_for_js(
            &page,
            "document.querySelector('crap-dirty-form')?._armed === true"
        )
        .await,
        "dirty form should arm itself before typing"
    );

    // Type into the title field
    page.find_element("input[name=\"title\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap()
        .type_str("Some title")
        .await
        .unwrap();

    // Poll until the input event flips _dirty to true
    assert!(
        browser::wait_for_js(
            &page,
            "document.querySelector('crap-dirty-form')?._dirty === true"
        )
        .await,
        "dirty form should be dirty after typing into a field"
    );

    // _dirty should now be true
    let result = page
        .evaluate("() => { const df = document.querySelector('crap-dirty-form'); return df ? df._dirty : null; }")
        .await
        .unwrap();
    let dirty: bool = result.into_value().unwrap_or(false);
    assert!(
        dirty,
        "dirty form should be dirty after typing into a field"
    );

    server_handle.abort();
}

// ── only_the_forms_own_accepted_save_clears_the_dirty_flag ─────────────

/// The dirty flag clears only when the server has ACCEPTED the form's own
/// save: a request cancelled at `htmx:beforeRequest` (pre-submit validation
/// that failed), a non-GET response for an unrelated element (an
/// inline-create panel), and the form's own save answered with an error
/// status (422 validation, 403) must all leave the page dirty, or the leave
/// prompt would be lost with the edits still unsaved.
#[tokio::test(flavor = "multi_thread")]
async fn only_the_forms_own_accepted_save_clears_the_dirty_flag() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_dirty_def(), make_users_def()],
        vec![],
        "bdirty4@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/posts/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    assert!(
        browser::wait_for_js(
            &page,
            "document.querySelector('crap-dirty-form')?._armed === true"
        )
        .await,
        "dirty form should arm itself before typing"
    );
    page.find_element("input[name=\"title\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap()
        .type_str("Some title")
        .await
        .unwrap();
    assert!(
        browser::wait_for_js(
            &page,
            "document.querySelector('crap-dirty-form')?._dirty === true"
        )
        .await,
        "dirty after typing"
    );

    let outcome = page
        .evaluate(
            r"() => {
                const df = document.querySelector('crap-dirty-form');
                const cfg = { requestConfig: { verb: 'post' } };
                // 1. A cancelled request (what pre-submit validation does).
                const cancelled = new CustomEvent('htmx:beforeRequest', {
                  bubbles: true, cancelable: true, detail: { elt: df.querySelector('#edit-form'), ...cfg },
                });
                cancelled.preventDefault();
                document.body.dispatchEvent(cancelled);
                const afterCancelled = df._dirty;
                // 2. A response to a request from an UNRELATED element.
                const onLoad = (elt, status) => document.body.dispatchEvent(
                  new CustomEvent('htmx:beforeOnLoad', {
                    bubbles: true, detail: { elt, xhr: { status }, ...cfg },
                  }),
                );
                const form = df.querySelector('#edit-form');
                onLoad(document.body, 200);
                const afterUnrelated = df._dirty;
                // 3. The form's own save, refused (validation / access).
                onLoad(form, 422);
                onLoad(form, 403);
                const afterRefused = df._dirty;
                // 4. The form's own save, accepted.
                onLoad(form, 200);
                const afterAccepted = df._dirty;
                return [afterCancelled, afterUnrelated, afterRefused, afterAccepted].join(',');
            }",
        )
        .await
        .unwrap();
    let outcome: String = outcome.into_value().unwrap();
    assert_eq!(
        outcome, "true,true,true,false",
        "only the form's own accepted save clears the flag"
    );

    server_handle.abort();
}

// ── a_drag_reorder_marks_the_form_dirty ──────────────────────────────────

fn make_array_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("lists");
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("label", FieldType::Text).build(),
            ])
            .build(),
    ];
    def
}

/// Regression: the dirty guard watched a click allow-list of row actions,
/// so a drag-and-drop reorder (no click, no input event) and a block added
/// from the card picker (a click inside a shadow root) left the form clean
/// and navigating away discarded them without a prompt. Every row mutation
/// now announces itself with `crap:change`.
#[tokio::test(flavor = "multi_thread")]
async fn a_drag_reorder_marks_the_form_dirty() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_array_def(), make_users_def()],
        vec![],
        "bdirty4@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/lists/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    assert!(
        browser::wait_for_js(
            &page,
            "document.querySelector('crap-dirty-form')?._armed === true"
        )
        .await,
        "dirty form should arm itself after load"
    );

    browser::click_until_element_count(
        &page,
        "button[data-action=\"add-array-row\"]",
        ".form__array-rows > .form__array-row",
        2,
    )
    .await;

    let result = page
        .evaluate(
            "() => { \
               const df = document.querySelector('crap-dirty-form'); \
               df._dirty = false; \
               const rows = document.querySelectorAll('.form__array-rows > .form__array-row'); \
               const handle = rows[1].querySelector('[draggable][data-drag]'); \
               const dt = new DataTransfer(); \
               handle.dispatchEvent(new DragEvent('dragstart', { bubbles: true, dataTransfer: dt })); \
               rows[0].dispatchEvent(new DragEvent('drop', \
                 { bubbles: true, cancelable: true, dataTransfer: dt, clientY: 0 })); \
               return df._dirty; \
             }",
        )
        .await
        .unwrap();
    let dirty: bool = result.into_value().unwrap_or(false);
    assert!(dirty, "a drag-and-drop reorder must mark the form dirty");

    server_handle.abort();
}

// ── unpublishing_a_dirty_form_asks_first ─────────────────────────────────

/// Regression: Unpublish submitted the whole edit form, which the server
/// ignored — unsaved edits were discarded without a word (and an unrelated
/// invalid field blocked the unpublish in pre-submit validation). With
/// unsaved edits the editor is now asked before anything is sent.
#[tokio::test(flavor = "multi_thread")]
async fn unpublishing_a_dirty_form_asks_first() {
    let mut def = make_dirty_def();
    def.versions = Some(VersionsConfig::new(true, 10));

    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        app,
        ..
    } = setup_browser_test(
        vec![def.clone(), make_users_def()],
        vec![],
        "bdirty5@test.com",
        "pass123",
    )
    .await;

    let id = {
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let data: DocumentFields = HashMap::from([("title".to_string(), json!("Live"))]).into();
        let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
        tx.commit().unwrap();
        doc.id.to_string()
    };

    page.goto(format!("{base_url}/admin/collections/posts/{id}"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    assert!(
        browser::wait_for_js(
            &page,
            "document.querySelector('crap-dirty-form')?._armed === true"
        )
        .await,
        "dirty form should arm itself after load"
    );

    page.evaluate(
        "() => { \
           const el = document.querySelector('[name=\"title\"]'); \
           el.value = 'Edited'; \
           el.dispatchEvent(new Event('input', { bubbles: true })); \
         }",
    )
    .await
    .unwrap();

    page.find_element("button[data-action=\"unpublish\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap();

    assert!(
        browser::wait_for_js(
            &page,
            "document.querySelector('crap-confirm-dialog')?.shadowRoot?.querySelector('dialog')?.open === true"
        )
        .await,
        "unpublishing a form with unsaved edits asks first"
    );

    server_handle.abort();
}
