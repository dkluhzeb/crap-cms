use std::time::Duration;

use crate::browser;
use crate::helpers::*;

use crap_cms::core::collection::*;
use crap_cms::core::field::LocalizedString;

fn make_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def
}

#[tokio::test(flavor = "multi_thread")]
async fn password_toggle_reveals_and_hides_value() {
    let (base_url, server_handle, _app) =
        browser::spawn_server(vec![make_def(), make_users_def()], vec![]).await;

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    page.goto(format!("{base_url}/admin/login")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let initial_type = page
        .evaluate("() => document.querySelector('crap-password-toggle input').type")
        .await
        .unwrap()
        .into_value::<String>()
        .unwrap();
    assert_eq!(initial_type, "password");

    let toggle_in_shadow = browser::shadow_eval(
        &page,
        "crap-password-toggle",
        "return root.querySelector('button.toggle') ? 'present' : 'missing';",
    )
    .await;
    assert_eq!(toggle_in_shadow, "present");

    page.evaluate(
        "() => document.querySelector('crap-password-toggle').shadowRoot \
         .querySelector('button.toggle').click()",
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let revealed_type = page
        .evaluate("() => document.querySelector('crap-password-toggle input').type")
        .await
        .unwrap()
        .into_value::<String>()
        .unwrap();
    assert_eq!(revealed_type, "text");

    page.evaluate(
        "() => document.querySelector('crap-password-toggle').shadowRoot \
         .querySelector('button.toggle').click()",
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let final_type = page
        .evaluate("() => document.querySelector('crap-password-toggle input').type")
        .await
        .unwrap()
        .into_value::<String>()
        .unwrap();
    assert_eq!(final_type, "password");

    server_handle.abort();
}

// Regression: the Material-Symbols `.material-symbols-outlined` rule lives
// in a document-level stylesheet (Google Fonts CSS) and does not pierce the
// shadow boundary, so the icon span needs the icon-font properties re-
// declared inside the component's adopted stylesheet — otherwise the user
// sees the literal ligature text "visibility" instead of the glyph.
#[tokio::test(flavor = "multi_thread")]
async fn password_toggle_icon_renders_with_icon_font() {
    let (base_url, server_handle, _app) =
        browser::spawn_server(vec![make_def(), make_users_def()], vec![]).await;

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    page.goto(format!("{base_url}/admin/login")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let font_family = browser::shadow_eval(
        &page,
        "crap-password-toggle",
        "const span = root.querySelector('span.material-symbols-outlined'); \
         return span ? getComputedStyle(span).fontFamily : '';",
    )
    .await;

    assert!(
        font_family.contains("Material Symbols Outlined"),
        "icon span must resolve to the Material Symbols Outlined font, got: {font_family:?}"
    );

    server_handle.abort();
}
