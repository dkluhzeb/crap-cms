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

use crap_cms::core::{collection::*, field::LocalizedString};
use crap_cms_e2e::{browser, helpers::*};

fn make_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def
}

// ── session_dialog_singleton_mounts ──────────────────────────────────────
//
// The `<crap-session-dialog>` singleton is rendered once in
// `templates/layout/base.hbs`. Verifies the element + shadow DOM are
// installed on any authenticated admin page.

#[tokio::test(flavor = "multi_thread")]
async fn session_dialog_singleton_mounts() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "bsess1@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "bsess1@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "bsess1@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/posts"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    sleep(Duration::from_millis(500)).await;

    let mount_state = browser::shadow_eval(
        &page,
        "crap-session-dialog",
        "return root.querySelector('dialog') ? 'has-dialog' : 'no-dialog';",
    )
    .await;
    assert_eq!(
        mount_state, "has-dialog",
        "crap-session-dialog should mount its <dialog> element"
    );

    server_handle.abort();
}

// ── session_dialog_show_displays_warning ─────────────────────────────────
//
// Drive the dialog's public `show()` API directly with a known message
// and verify the message text + action buttons render. The real warning
// is normally triggered by the `crap_session_exp` cookie reaching ≤ 5min
// of remaining life; this test bypasses the timing logic so it can run
// deterministically in seconds rather than minutes.

#[tokio::test(flavor = "multi_thread")]
async fn session_dialog_show_displays_warning() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "bsess2@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "bsess2@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "bsess2@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/posts"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    sleep(Duration::from_millis(500)).await;

    // Trigger the dialog via its public show() API with a known message.
    page.evaluate(
        "() => { \
         const el = document.querySelector('crap-session-dialog'); \
         el?.show('TEST-WARNING-MESSAGE', { \
           onStay: () => {}, \
           onLogout: () => {}, \
         }); \
       }",
    )
    .await
    .unwrap();
    sleep(Duration::from_millis(200)).await;

    let is_open = browser::shadow_eval(
        &page,
        "crap-session-dialog",
        "return root.querySelector('dialog')?.hasAttribute('open') ? 'true' : 'false';",
    )
    .await;
    assert_eq!(is_open, "true", "dialog should be open after show()");

    let body_text = browser::shadow_eval(
        &page,
        "crap-session-dialog",
        "return root.querySelector('.body p')?.textContent || '';",
    )
    .await;
    assert!(
        body_text.contains("TEST-WARNING-MESSAGE"),
        "dialog body should contain the warning message, got: {body_text:?}"
    );

    let has_stay = browser::shadow_eval(
        &page,
        "crap-session-dialog",
        "return root.querySelector('button.stay') ? 'yes' : 'no';",
    )
    .await;
    assert_eq!(
        has_stay, "yes",
        "dialog should have a Stay-logged-in button"
    );

    let has_logout = browser::shadow_eval(
        &page,
        "crap-session-dialog",
        "return root.querySelector('button.logout') ? 'yes' : 'no';",
    )
    .await;
    assert_eq!(has_logout, "yes", "dialog should have a Log-out button");

    server_handle.abort();
}
