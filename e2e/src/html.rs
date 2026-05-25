use scraper::{ElementRef, Html, Selector};

/// Parse an HTML string into a document.
pub fn parse(body: &str) -> Html {
    Html::parse_document(body)
}

/// Select a single element matching `selector`. Panics if not found.
pub fn select_one<'a>(doc: &'a Html, selector: &str) -> ElementRef<'a> {
    let sel =
        Selector::parse(selector).unwrap_or_else(|e| panic!("bad selector {selector:?}: {e:?}"));
    doc.select(&sel)
        .next()
        .unwrap_or_else(|| panic!("expected element matching {selector:?}, found none"))
}

/// Select all elements matching `selector`.
pub fn select_all<'a>(doc: &'a Html, selector: &str) -> Vec<ElementRef<'a>> {
    let sel =
        Selector::parse(selector).unwrap_or_else(|e| panic!("bad selector {selector:?}: {e:?}"));
    doc.select(&sel).collect()
}

/// Assert that at least one element matching `selector` exists.
pub fn assert_exists(doc: &Html, selector: &str, msg: &str) {
    let sel =
        Selector::parse(selector).unwrap_or_else(|e| panic!("bad selector {selector:?}: {e:?}"));
    assert!(
        doc.select(&sel).next().is_some(),
        "{msg}: no element matching {selector:?}"
    );
}

/// Assert that no element matching `selector` exists.
pub fn assert_not_exists(doc: &Html, selector: &str, msg: &str) {
    let sel =
        Selector::parse(selector).unwrap_or_else(|e| panic!("bad selector {selector:?}: {e:?}"));
    assert!(
        doc.select(&sel).next().is_none(),
        "{msg}: unexpected element matching {selector:?}"
    );
}

/// Assert a field wrapper with `data-field-name="field_name"` exists.
pub fn assert_field_exists(doc: &Html, field_name: &str) {
    assert_exists(
        doc,
        &format!("[data-field-name=\"{field_name}\"]"),
        &format!("field {field_name:?}"),
    );
}

/// Assert an `<input>` with given `name`, `type`, and optionally `value` exists.
pub fn assert_input(doc: &Html, name: &str, input_type: &str, value: Option<&str>) {
    let selector = format!("input[name=\"{name}\"][type=\"{input_type}\"]");
    let el = select_one(doc, &selector);
    if let Some(expected) = value {
        let actual = el.value().attr("value").unwrap_or("");
        assert_eq!(
            actual, expected,
            "input[name={name:?}] value: expected {expected:?}, got {actual:?}"
        );
    }
}

/// Assert a field error exists within the `data-field-name` wrapper.
pub fn assert_field_error(doc: &Html, field_name: &str, msg_contains: &str) {
    let selector = format!("[data-field-name=\"{field_name}\"] .form__error");
    let el = select_one(doc, &selector);
    let text = el.text().collect::<String>();
    assert!(
        text.to_lowercase().contains(&msg_contains.to_lowercase()),
        "field {field_name:?} error: expected text containing {msg_contains:?}, got {text:?}"
    );
}

/// Assert no error exists within the `data-field-name` wrapper.
pub fn assert_no_field_error(doc: &Html, field_name: &str) {
    assert_not_exists(
        doc,
        &format!("[data-field-name=\"{field_name}\"] .form__error"),
        &format!("field {field_name:?} should have no error"),
    );
}

/// Count elements matching `selector`.
pub fn count(doc: &Html, selector: &str) -> usize {
    let sel =
        Selector::parse(selector).unwrap_or_else(|e| panic!("bad selector {selector:?}: {e:?}"));
    doc.select(&sel).count()
}

/// Get the concatenated text content of the first element matching `selector`.
pub fn text_of(doc: &Html, selector: &str) -> String {
    select_one(doc, selector).text().collect()
}
