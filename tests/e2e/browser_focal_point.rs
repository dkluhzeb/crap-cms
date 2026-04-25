use std::time::Duration;

use crate::browser;
use crate::helpers::*;

use crap_cms::core::collection::*;
use crap_cms::core::field::*;
use crap_cms::core::upload::CollectionUpload;

fn make_media_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("media");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Media".to_string())),
        plural: Some(LocalizedString::Plain("Media".to_string())),
    };
    def.timestamps = true;
    def.upload = Some(CollectionUpload::new());
    def.fields = vec![FieldDefinition::builder("alt", FieldType::Text).build()];
    def
}

// ── focal_point_click_updates_inputs ─────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn focal_point_click_updates_inputs() {
    let (base_url, server_handle, app) =
        browser::spawn_server(vec![make_media_def(), make_users_def()], vec![]).await;
    let user_id = create_test_user(&app, "bfocal@test.com", "pass123");
    let _ = make_auth_cookie(&app, &user_id, "bfocal@test.com");

    // We can't easily upload a real image in tests. Instead, navigate to the
    // create page and inject a mock <crap-focal-point> with an image via JS
    // to test the click-to-set-focal-point behavior.

    let (browser, _browser_handle) = browser::launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();

    browser::browser_login(&page, &base_url, "bfocal@test.com", "pass123").await;

    // Navigate to any admin page
    page.goto(format!("{base_url}/admin/collections/media"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Inject a mock focal-point component into the page for testing
    page.evaluate(
        "() => { \
            document.body.innerHTML += `\
                <crap-focal-point data-focal-x=\"0.5\" data-focal-y=\"0.5\">\
                    <div class=\"focal-point\" style=\"position:relative;width:400px;height:300px;\">\
                        <img src=\"data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==\" \
                             style=\"width:400px;height:300px;display:block;\" />\
                        <div class=\"focal-point__marker\" style=\"position:absolute;\"></div>\
                    </div>\
                    <input type=\"hidden\" name=\"focal_x\" value=\"0.5000\" />\
                    <input type=\"hidden\" name=\"focal_y\" value=\"0.5000\" />\
                </crap-focal-point>`; \
        }",
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // The shadow `<img>` gets its src from the light-DOM template above, but
    // its natural size is 1×1 (the test fixture is a 1×1 PNG). Force
    // testable dimensions so getBoundingClientRect returns non-zero.
    page.evaluate(
        "() => { \
            const fp = document.querySelector('crap-focal-point'); \
            const img = fp.shadowRoot.querySelector('img'); \
            img.style.width = '400px'; \
            img.style.height = '300px'; \
            img.style.maxWidth = 'none'; \
            img.style.maxHeight = 'none'; \
        }",
    )
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Simulate a click at a specific position (top-left quadrant)
    page.evaluate(
        "() => { \
            const fp = document.querySelector('crap-focal-point'); \
            const img = fp.shadowRoot.querySelector('img'); \
            const rect = img.getBoundingClientRect(); \
            const clickX = rect.left + rect.width * 0.25; \
            const clickY = rect.top + rect.height * 0.25; \
            img.dispatchEvent(new MouseEvent('click', { \
                clientX: clickX, clientY: clickY, bubbles: true \
            })); \
        }",
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Check that hidden inputs were updated (should be near 0.25)
    let result = page
        .evaluate("() => document.querySelector('input[name=\"focal_x\"]')?.value ?? ''")
        .await
        .unwrap();
    let focal_x: String = result.into_value().unwrap();

    let result = page
        .evaluate("() => document.querySelector('input[name=\"focal_y\"]')?.value ?? ''")
        .await
        .unwrap();
    let focal_y: String = result.into_value().unwrap();

    // Values should have changed from the initial 0.5
    let x: f64 = focal_x.parse().unwrap_or(0.5);
    let y: f64 = focal_y.parse().unwrap_or(0.5);

    assert!(
        (x - 0.25).abs() < 0.15,
        "focal_x should be near 0.25 after clicking top-left quadrant, got: {x}"
    );
    assert!(
        (y - 0.25).abs() < 0.15,
        "focal_y should be near 0.25 after clicking top-left quadrant, got: {y}"
    );

    server_handle.abort();
}
