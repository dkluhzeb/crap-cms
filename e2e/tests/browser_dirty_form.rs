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
use crap_cms::core::{collection::*, field::*};

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

// ── only_the_forms_own_sent_request_clears_the_dirty_flag ───────────────

/// The dirty flag clears only when the form's OWN request is actually sent:
/// a request cancelled at `htmx:beforeRequest` (pre-submit validation that
/// failed) and a non-GET request from an unrelated element (an inline-create
/// panel) must leave the page dirty, or the leave prompt would be lost.
#[tokio::test(flavor = "multi_thread")]
async fn only_the_forms_own_sent_request_clears_the_dirty_flag() {
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
                // 2. A request actually sent by an UNRELATED element.
                document.body.dispatchEvent(new CustomEvent('htmx:beforeSend', {
                  bubbles: true, detail: { elt: document.body, ...cfg },
                }));
                const afterUnrelated = df._dirty;
                // 3. The form's own request actually sent.
                document.body.dispatchEvent(new CustomEvent('htmx:beforeSend', {
                  bubbles: true, detail: { elt: df.querySelector('#edit-form'), ...cfg },
                }));
                const afterOwnSend = df._dirty;
                return [afterCancelled, afterUnrelated, afterOwnSend].join(',');
            }",
        )
        .await
        .unwrap();
    let outcome: String = outcome.into_value().unwrap();
    assert_eq!(
        outcome, "true,true,false",
        "cancelled and unrelated requests keep the flag; only the own send clears it"
    );

    server_handle.abort();
}
