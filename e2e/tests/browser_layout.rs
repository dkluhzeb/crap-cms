#![allow(
    clippy::missing_panics_doc,
    clippy::too_many_lines,
    clippy::used_underscore_binding
)]
//! `admin.width` lays fields out in rows in the edit form.

use chromiumoxide::Page;

use crap_cms::core::{collection::*, field::*};

use crap_cms_e2e::{BrowserTestCtx, browser, helpers::*, setup_browser_test};

fn text(name: &str, width: Option<&str>) -> FieldDefinition {
    let mut admin = FieldAdminBuilder::new();
    if let Some(width) = width {
        admin = admin.width(width);
    }

    FieldDefinition::builder(name, FieldType::Text)
        .admin(admin.build())
        .build()
}

/// Two half-width fields and a full-width one at the top level; two `50%`
/// fields (a custom CSS width) inside an expanded collapsible.
fn make_layout_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("layouts");
    def.timestamps = true;
    def.fields = vec![
        text("first", Some("half")),
        text("second", Some("half")),
        text("third", None),
        FieldDefinition::builder("details", FieldType::Collapsible)
            .admin(FieldAdminBuilder::new().collapsed(false).build())
            .fields(vec![text("left", Some("50%")), text("right", Some("50%"))])
            .build(),
    ];
    def
}

/// `"a,b,c,d,e"`: the top offset of each named field's wrapper, after the
/// main column is set to `width`.
const TOPS_JS: &str = "(width) => { \
    document.querySelector('.edit-layout__content').style.width = width; \
    const top = (n) => Math.round(document.querySelector(`[data-field-name=\"${n}\"]`) \
        .getBoundingClientRect().top); \
    return ['first', 'second', 'third', 'left', 'right'].map(top).join(','); }";

async fn tops(page: &Page, width: &str) -> Vec<i64> {
    let js = format!("() => ({TOPS_JS})('{width}')");
    let joined: String = page
        .evaluate(js.as_str())
        .await
        .unwrap()
        .into_value()
        .unwrap();

    joined.split(',').map(|t| t.parse().unwrap()).collect()
}

/// Regression: `admin.width` had no effect on the edit form. Narrowed fields
/// now share a row — a named width by its class, a custom CSS width through
/// the field-width script — and stack again in a narrow container.
#[tokio::test(flavor = "multi_thread")]
async fn narrowed_fields_share_a_row_and_stack_when_narrow() {
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        ..
    } = setup_browser_test(
        vec![make_layout_def(), make_users_def()],
        vec![],
        "blayout1@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/layouts/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    browser::wait_for_js(
        &page,
        "document.querySelector('[data-field-name=\"left\"]')?.style.getPropertyValue('--field-basis')",
    )
    .await;

    let wide = tops(&page, "1000px").await;
    assert_eq!(wide[0], wide[1], "half + half share a row: {wide:?}");
    assert!(
        wide[2] > wide[0],
        "a full-width field starts a new row: {wide:?}"
    );
    assert_eq!(wide[3], wide[4], "50% + 50% share a row: {wide:?}");

    let narrow = tops(&page, "400px").await;
    assert!(
        narrow[1] > narrow[0],
        "half fields stack when narrow: {narrow:?}"
    );
    assert!(
        narrow[4] > narrow[3],
        "custom widths stack when narrow: {narrow:?}"
    );

    server_handle.abort();
}
