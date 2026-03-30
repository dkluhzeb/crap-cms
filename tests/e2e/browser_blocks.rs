use std::time::Duration;

use std::collections::HashMap;

use crate::browser;
use crate::helpers::*;

use crap_cms::core::collection::*;
use crap_cms::core::field::*;
use crap_cms::db::query;

fn make_blocks_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("pages");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Page".to_string())),
        plural: Some(LocalizedString::Plain("Pages".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![
                BlockDefinition::new(
                    "text_block",
                    vec![FieldDefinition::builder("body", FieldType::Textarea).build()],
                ),
                BlockDefinition::new(
                    "image_block",
                    vec![FieldDefinition::builder("alt", FieldType::Text).build()],
                ),
            ])
            .build(),
    ];
    def
}

// ── block_picker_shows_options ───────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn block_picker_shows_options() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_blocks_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "bblock1@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "bblock1@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "bblock1@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/pages/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // The block type select should have options for both block types
    let result = page
        .evaluate("() => document.querySelector('.form__blocks-select')?.options.length ?? 0")
        .await
        .unwrap();
    let option_count: i64 = result.into_value().unwrap();
    assert!(
        option_count >= 2,
        "block picker select should have at least 2 options, got {option_count}"
    );

    server_handle.abort();
}

// ── block_picker_adds_block ──────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn block_picker_adds_block() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_blocks_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "bblock2@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "bblock2@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "bblock2@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/pages/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Initially no rows
    let rows = page.find_elements(".form__array-row").await.unwrap();
    assert_eq!(rows.len(), 0, "should start with 0 block rows");

    // Click add block button
    page.find_element("button[data-action=\"add-block-row\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let rows = page.find_elements(".form__array-row").await.unwrap();
    assert_eq!(rows.len(), 1, "should have 1 block row after adding");

    server_handle.abort();
}

// ── blocks_remove_block ──────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn blocks_remove_block() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_blocks_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "bblock3@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "bblock3@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "bblock3@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/pages/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Add a block
    page.find_element("button[data-action=\"add-block-row\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Remove the block
    page.find_element("button[data-action=\"remove-array-row\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let rows = page.find_elements(".form__array-row").await.unwrap();
    assert_eq!(rows.len(), 0, "block row should be removed");

    server_handle.abort();
}

// ── blocks_different_types ───────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn blocks_different_types() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_blocks_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "bblock4@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "bblock4@test.com");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "bblock4@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/pages/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Add first block (text_block — default selected)
    page.find_element("button[data-action=\"add-block-row\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Switch select to image_block and add second block
    page.evaluate(
        "() => { const sel = document.querySelector('.form__blocks-select'); sel.value = 'image_block'; }",
    )
    .await
    .unwrap();

    page.find_element("button[data-action=\"add-block-row\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let rows = page.find_elements(".form__array-row").await.unwrap();
    assert_eq!(rows.len(), 2, "should have 2 block rows of different types");

    // Check that we have both block types via hidden inputs
    let result = page
        .evaluate(
            "() => [...document.querySelectorAll('input[name*=\"_block_type\"]')].map(i => i.value)",
        )
        .await
        .unwrap();
    let block_types: Vec<String> = result.into_value().unwrap();
    assert!(
        block_types.contains(&"text_block".to_string()),
        "should have a text_block, got: {block_types:?}"
    );
    assert!(
        block_types.contains(&"image_block".to_string()),
        "should have an image_block, got: {block_types:?}"
    );

    server_handle.abort();
}

// ── Regression: block with relationship sub-field (__INDEX__ replacement) ─

fn make_blocks_with_rel_def() -> CollectionDefinition {
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
        FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![BlockDefinition::new(
                "hero",
                vec![
                    FieldDefinition::builder("heading", FieldType::Text).build(),
                    FieldDefinition::builder("image", FieldType::Relationship)
                        .relationship(RelationshipConfig::new("media", false))
                        .build(),
                ],
            )])
            .build(),
    ];
    def
}

fn make_media_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("media");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Media".to_string())),
        plural: Some(LocalizedString::Plain("Media".to_string())),
    };
    def.timestamps = true;
    def.admin = AdminConfig {
        use_as_title: Some("filename".to_string()),
        ..Default::default()
    };
    def.fields = vec![
        FieldDefinition::builder("filename", FieldType::Text)
            .required(true)
            .build(),
    ];
    def
}

fn create_media(app: &TestApp, filename: &str) -> String {
    let reg = app.registry.read().unwrap();
    let def = reg.get_collection("media").unwrap().clone();
    drop(reg);

    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = HashMap::from([("filename".to_string(), filename.to_string())]);
    let doc = query::create(&tx, "media", &def, &data, None).unwrap();
    tx.commit().unwrap();
    doc.id.to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn block_with_relationship_saves_correctly() {
    let (base_url, server_handle, app) = browser::spawn_server(
        vec![
            make_blocks_with_rel_def(),
            make_media_def(),
            make_users_def(),
        ],
        vec![],
    )
    .await;
    let user_id = create_test_user(&app, "bblockrel@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "bblockrel@test.com");

    let _media_id = create_media(&app, "hero.jpg");

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "bblockrel@test.com", "pass123").await;

    page.goto(format!("{base_url}/admin/collections/articles/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Fill title
    page.find_element("input[name=\"title\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap()
        .type_str("Block Rel Test")
        .await
        .unwrap();

    // Add a hero block
    page.evaluate(
        "() => { \
            const btn = document.querySelector('button[data-action=\"add-block-row\"]'); \
            if (btn) btn.click(); \
        }",
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Fill the heading field inside the block
    page.evaluate(
        "() => { \
            const input = document.querySelector('input[name*=\"heading\"]'); \
            if (input) { input.focus(); input.value = 'Hero Title'; \
                input.dispatchEvent(new Event('input', {bubbles: true})); } \
        }",
    )
    .await
    .unwrap();

    // Search for media in the relationship field inside the block
    page.evaluate(
        "() => { \
            const relInput = document.querySelector('.form__array-row .relationship-search__input'); \
            if (relInput) { relInput.focus(); } \
        }",
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;

    // Select first option
    page.evaluate(
        "() => { \
            const opt = document.querySelector('.form__array-row .relationship-search__option'); \
            if (opt) opt.dispatchEvent(new MouseEvent('mousedown', {bubbles: true})); \
        }",
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Verify the hidden input has the correct field name (not __INDEX__)
    let field_name_result = page
        .evaluate(
            "() => { \
                const hidden = document.querySelector('.form__array-row input[type=\"hidden\"][name*=\"image\"]'); \
                return hidden ? hidden.name : 'NOT_FOUND'; \
            }",
        )
        .await
        .unwrap();
    let field_name: String = field_name_result.into_value().unwrap_or_default();
    assert!(
        !field_name.contains("__INDEX__"),
        "field name should not contain __INDEX__ placeholder, got: {field_name}"
    );
    assert!(
        field_name.contains("[0]"),
        "field name should contain [0] index, got: {field_name}"
    );

    server_handle.abort();
}
