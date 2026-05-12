use std::collections::HashMap;
use std::time::Duration;

use crate::browser;
use crate::helpers::*;

use crap_cms::core::DocumentFields;
use crap_cms::core::collection::*;
use crap_cms::core::field::*;
use crap_cms::db::query;
use serde_json::json;

fn make_time_def() -> CollectionDefinition {
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

// ── time_element_renders_formatted ───────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn time_element_renders_formatted() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_time_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "btime@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "btime@test.com");

    // Create a document so the list has a row with a <crap-time> element
    {
        let reg = app.registry.read().unwrap();
        let def = reg.get_collection("posts").unwrap().clone();

        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let data: DocumentFields =
            HashMap::from([("title".to_string(), json!("Time Test Post"))]).into();
        query::create(&tx, "posts", &def, &data, None).unwrap();
        tx.commit().unwrap();
    }

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "btime@test.com", "pass123").await;

    // Navigate to list page where <crap-time> elements are rendered
    page.goto(format!("{base_url}/admin/collections/posts"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // <crap-time> should contain formatted text, not empty or raw ISO
    let result = page
        .evaluate("() => { const el = document.querySelector('crap-time'); return el ? el.textContent.trim() : ''; }")
        .await
        .unwrap();
    let time_text: String = result.into_value().unwrap();
    assert!(
        !time_text.is_empty(),
        "crap-time should render non-empty formatted text"
    );
    // The formatted text should not be a raw ISO string (it should have spaces, commas, etc.)
    assert!(
        !time_text.starts_with("20") || time_text.contains(',') || time_text.contains(' '),
        "crap-time should render human-readable format, got: {time_text}"
    );

    server_handle.abort();
}
