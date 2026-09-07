//! Client-side display conditions (a condition hook that returns a table) —
//! the table must re-evaluate instantly for every control shape, including
//! a radio group, where the change event fires on the option that was
//! clicked rather than on the first input carrying the name.

use std::fs;

use crap_cms::config::CrapConfig;
use crap_cms::core::{collection::*, field::*};
use crap_cms_e2e::{BrowserTestCtx, browser, helpers::*, setup_browser_test_at};

fn make_posts_def() -> CollectionDefinition {
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
        FieldDefinition::builder("post_type", FieldType::Radio)
            .options(vec![
                SelectOption::new(LocalizedString::Plain("Article".into()), "article"),
                SelectOption::new(LocalizedString::Plain("Link".into()), "link"),
            ])
            .build(),
        FieldDefinition::builder("link_url", FieldType::Text)
            .admin(
                FieldAdmin::builder()
                    .condition("hooks.conditions.link_only")
                    .build(),
            )
            .build(),
    ];
    def
}

/// Boot with a `hooks/conditions/link_only.lua` that returns a condition
/// TABLE (client-evaluated), not a boolean.
fn prepared_config_dir() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let hooks_dir = tmp.path().join("hooks").join("conditions");
    fs::create_dir_all(&hooks_dir).expect("mkdir hooks/conditions");
    fs::write(
        hooks_dir.join("link_only.lua"),
        r#"
return function(_ctx)
    return { field = "post_type", equals = "link" }
end
"#,
    )
    .expect("write hook file");
    tmp
}

const LINK_URL_HIDDEN: &str = "document.querySelector('[data-field-name=\"link_url\"]')?.classList.contains('form__field--hidden')";

#[tokio::test(flavor = "multi_thread")]
async fn radio_group_drives_a_client_side_condition() {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    // `app` owns the temp config dir the condition hook is loaded from at
    // request time — bind it (a `..` pattern would drop it, deleting the dir
    // and silently un-resolving the hook), like `browser` keeps CDP alive.
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        app: _app,
        ..
    } = setup_browser_test_at(
        vec![make_posts_def(), make_users_def()],
        vec![],
        config,
        prepared_config_dir(),
        "bcond1@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/posts/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Nothing selected yet: the conditioned field starts hidden.
    assert!(
        browser::wait_for_js(&page, &format!("{LINK_URL_HIDDEN} === true")).await,
        "link_url is hidden until post_type = link"
    );
    browser::wait_for_js(&page, "customElements.get('crap-conditions')").await;

    // Clicking the SECOND radio (not the first, which is the only one a
    // first-match listener would watch) must reveal the field instantly.
    page.find_element("input[name=\"post_type\"][value=\"link\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap();
    assert!(
        browser::wait_for_js(&page, &format!("{LINK_URL_HIDDEN} === false")).await,
        "link_url appears once the 'link' radio is chosen"
    );

    // And switching back hides it again.
    page.find_element("input[name=\"post_type\"][value=\"article\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap();
    assert!(
        browser::wait_for_js(&page, &format!("{LINK_URL_HIDDEN} === true")).await,
        "link_url hides again for 'article'"
    );

    server_handle.abort();
}
