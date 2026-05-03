use std::time::Duration;

use crate::browser;
use crate::helpers::*;

use crap_cms::core::collection::*;
use crap_cms::core::field::*;

fn make_tags_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("articles");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Article".to_string())),
        plural: Some(LocalizedString::Plain("Articles".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("keywords", FieldType::Text)
            .has_many(true)
            .build(),
    ];
    def
}

/// Drive the `<crap-tags>` shadow input by setting `.value` and dispatching
/// a synthetic `keydown` Enter — the component's listener mutates state from
/// either path. We can't use `page.find_element(".tags__input")` because the
/// element lives in Shadow DOM (closed-from-CSS-perspective for `querySelector`).
async fn add_tag(page: &chromiumoxide::Page, value: &str) {
    let js = format!(
        "() => {{ \
            const host = document.querySelector('crap-tags'); \
            const input = host.shadowRoot.querySelector('.tags__input'); \
            input.focus(); \
            input.value = {value}; \
            input.dispatchEvent(new Event('input')); \
            input.dispatchEvent(new KeyboardEvent('keydown', {{ key: 'Enter', bubbles: true }})); \
            return 'ok'; \
        }}",
        value = serde_json::to_string(value).unwrap(),
    );

    page.evaluate(js.as_str()).await.unwrap();
}

async fn chip_count(page: &chromiumoxide::Page) -> i64 {
    page.evaluate(
        "() => document.querySelector('crap-tags').shadowRoot.querySelectorAll('.chip').length",
    )
    .await
    .unwrap()
    .into_value()
    .unwrap_or(0)
}

// ── tags_add_via_enter ───────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn tags_add_via_enter() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_tags_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "btag1@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "btag1@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "btag1@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/articles/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    add_tag(&page, "rust").await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        chip_count(&page).await,
        1,
        "should have 1 tag chip after pressing Enter"
    );

    server_handle.abort();
}

// ── tags_remove_via_click ────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn tags_remove_via_click() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_tags_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "btag2@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "btag2@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "btag2@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/articles/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    add_tag(&page, "removeme").await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Click the chip's remove button via shadow root.
    page.evaluate(
        "() => document.querySelector('crap-tags').shadowRoot.querySelector('.chip__remove').click()",
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        chip_count(&page).await,
        0,
        "chip should be removed after clicking X"
    );

    server_handle.abort();
}

// ── tags_prevent_duplicates ──────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn tags_prevent_duplicates() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_tags_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "btag3@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "btag3@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "btag3@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/articles/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    for _ in 0..2 {
        add_tag(&page, "duplicate").await;
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    assert_eq!(
        chip_count(&page).await,
        1,
        "duplicate tags should be prevented"
    );

    server_handle.abort();
}

// ── tags_submit_persists ─────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn tags_submit_persists() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_tags_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "btag4@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "btag4@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "btag4@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/articles/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Title is light-DOM, can use find_element directly.
    page.find_element("input[name=\"title\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap()
        .type_str("Tag Article")
        .await
        .unwrap();

    for tag in &["alpha", "beta"] {
        add_tag(&page, tag).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Hidden input is in light DOM (slotted from outside the shadow root).
    let hidden_val: String = page
        .evaluate("() => document.querySelector('crap-tags input[type=\"hidden\"]')?.value ?? ''")
        .await
        .unwrap()
        .into_value()
        .unwrap();
    assert!(
        hidden_val.contains("alpha"),
        "hidden input should contain 'alpha', got: {hidden_val}"
    );
    assert!(
        hidden_val.contains("beta"),
        "hidden input should contain 'beta', got: {hidden_val}"
    );

    server_handle.abort();
}
