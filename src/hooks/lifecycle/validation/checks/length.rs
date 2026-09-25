use serde_json::Value;

use crate::core::{
    FieldDefinition, FieldType,
    richtext::{richtext_is_blank, richtext_plain_text},
    validate::FieldError,
};

/// Whether `field` stores text: a present value must be a string, or the
/// write would store the JSON spelling of whatever was sent (`["a"]` as
/// `"[\"a\"]"`).
fn stores_text(field: &FieldDefinition) -> bool {
    matches!(
        field.field_type,
        FieldType::Text | FieldType::Textarea | FieldType::Email | FieldType::Code
    )
}

/// The error for a present value on a text field that isn't text.
pub(crate) fn not_text_error(field: &FieldDefinition, data_key: &str) -> FieldError {
    FieldError::with_key(
        data_key.to_owned(),
        format!("{} must be text", field.name),
        "validation.invalid_text",
    )
    .with_param("field", field.name.clone())
}

/// The length `min_length` / `max_length` measure: the characters of a text
/// value, or of a rich text value's plain text (either format, a JSON document
/// as text or as an object). `Err` when the value is not text of the field's
/// kind; `Ok(None)` — nothing to measure — for blank rich text (absent, like
/// an empty string: bounds never make an optional field required) and for
/// rich text unreadable in its format, which the rich text shape check
/// refuses on its own.
fn measured_length(field: &FieldDefinition, value: Option<&Value>) -> Result<Option<usize>, ()> {
    if field.field_type == FieldType::Richtext {
        return Ok(value
            .filter(|v| !richtext_is_blank(field, v))
            .and_then(|v| richtext_plain_text(field, v))
            .map(|text| text.chars().count()));
    }

    match value {
        Some(Value::String(s)) => Ok(Some(s.chars().count())),
        _ => Err(()),
    }
}

/// Reject a present value on a text field that isn't a string, and validate
/// `min_length` / `max_length` — on the string, or on a rich text value's
/// plain text (markup is not counted). Skipped for `has_many` fields
/// (validated per-element in `check_has_many_elements`).
pub(crate) fn check_length_bounds(
    field: &FieldDefinition,
    data_key: &str,
    value: Option<&Value>,
    is_empty: bool,
    errors: &mut Vec<FieldError>,
) {
    let has_bounds = field.min_length.is_some() || field.max_length.is_some();

    if is_empty || field.has_many || (!stores_text(field) && !has_bounds) {
        return;
    }

    let Ok(len) = measured_length(field, value) else {
        // A present non-string value would be stored as its JSON spelling —
        // and on a length-constrained field bypass min/max_length. Reject it,
        // matching how a present non-numeric value is rejected for Number.
        errors.push(not_text_error(field, data_key));

        return;
    };

    let Some(len) = len.filter(|_| has_bounds) else {
        return;
    };

    if let Some(min_len) = field.min_length
        && len < min_len
    {
        errors.push(
            FieldError::with_key(
                data_key.to_owned(),
                format!("{} must be at least {} characters", field.name, min_len),
                "validation.min_length",
            )
            .with_param("field", field.name.clone())
            .with_param("min", min_len.to_string()),
        );
    }

    if let Some(max_len) = field.max_length
        && len > max_len
    {
        errors.push(
            FieldError::with_key(
                data_key.to_owned(),
                format!("{} must be at most {} characters", field.name, max_len),
                "validation.max_length",
            )
            .with_param("field", field.name.clone())
            .with_param("max", max_len.to_string()),
        );
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use crate::core::DocumentFields;
    use crate::core::{FieldAdmin, FieldDefinition, FieldType};
    use crate::hooks::lifecycle::validation::{ValidationCtx, validate_fields_inner};
    use serde_json::{Value, json};

    /// The error keys of validating `value` on a field of `field_type` named
    /// `name`, with no other constraint.
    fn keys_for(field_type: FieldType, value: Value) -> Vec<String> {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, v TEXT)")
            .unwrap();
        let fields = vec![FieldDefinition::builder("v", field_type).build()];
        let mut data = DocumentFields::new();
        data.insert("v".to_string(), value);

        validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        )
        .err()
        .map(|e| e.errors.iter().filter_map(|fe| fe.key.clone()).collect())
        .unwrap_or_default()
    }

    /// Regression: without a length bound, `{"title": ["a"]}` on a text field
    /// was stored as `"[\"a\"]"`. A present value on a text-storing field must
    /// be a string whether or not a bound is set; null and empty stay absence.
    #[test]
    fn a_non_string_value_on_a_text_field_is_always_rejected() {
        for field_type in [
            FieldType::Text,
            FieldType::Textarea,
            FieldType::Email,
            FieldType::Code,
        ] {
            for value in [json!(["a"]), json!(42), json!(true), json!({ "a": 1 })] {
                assert!(
                    keys_for(field_type.clone(), value.clone())
                        .contains(&"validation.invalid_text".to_string()),
                    "{field_type:?} must reject {value}"
                );
            }

            for value in [json!("a"), Value::Null, json!("")] {
                assert!(
                    !keys_for(field_type.clone(), value.clone())
                        .contains(&"validation.invalid_text".to_string()),
                    "{field_type:?} must accept {value}"
                );
            }
        }

        assert!(
            keys_for(FieldType::Json, json!(["a"])).is_empty(),
            "a JSON field takes any value"
        );
    }

    /// Regression: a present non-string value on a length-constrained field was
    /// silently coerced to its stringified form, bypassing `min/max_length`. It
    /// must be rejected, symmetric with Number's present-but-wrong-typed rule.
    #[test]
    fn non_string_value_on_length_constrained_field_rejected() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("name", FieldType::Text)
                .min_length(5)
                .build(),
        ];
        // `12` would stringify to "12" (2 chars) and sneak past min_length=5.
        let mut data = DocumentFields::new();
        data.insert("name".to_string(), json!(12));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "a non-string value must be rejected, not coerced past min_length"
        );
        assert!(
            result
                .unwrap_err()
                .errors
                .iter()
                .any(|e| e.key.as_deref() == Some("validation.invalid_text"))
        );
    }

    #[test]
    fn test_validate_min_length_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("name", FieldType::Text)
                .min_length(5)
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("name".to_string(), json!("ab"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        assert!(
            result.unwrap_err().errors[0]
                .message
                .contains("at least 5 characters")
        );
    }

    #[test]
    fn test_validate_min_length_passes() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("name", FieldType::Text)
                .min_length(3)
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("name".to_string(), json!("hello"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_max_length_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("name", FieldType::Text)
                .max_length(5)
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("name".to_string(), json!("toolongvalue"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        assert!(
            result.unwrap_err().errors[0]
                .message
                .contains("at most 5 characters")
        );
    }

    #[test]
    fn test_validate_max_length_passes() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("name", FieldType::Text)
                .max_length(10)
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("name".to_string(), json!("short"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok());
    }

    /// Regression: length validation must count characters, not bytes.
    /// Multibyte UTF-8 characters (emoji, CJK, accented) were overcounted.
    #[test]
    fn test_validate_length_counts_chars_not_bytes() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)")
            .unwrap();

        // "café" = 4 chars but 5 bytes (é is 2 bytes)
        let fields = vec![
            FieldDefinition::builder("name", FieldType::Text)
                .max_length(4)
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("name".to_string(), json!("café"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok(), "café is 4 chars — should pass max_length=4");

        // "你好世界" = 4 chars but 12 bytes
        let fields = vec![
            FieldDefinition::builder("name", FieldType::Text)
                .min_length(4)
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("name".to_string(), json!("你好世界"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_ok(),
            "你好世界 is 4 chars — should pass min_length=4"
        );
    }

    #[test]
    fn test_validate_min_max_length_skipped_for_empty() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("name", FieldType::Text)
                .min_length(5)
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("name".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_ok(),
            "min_length should not trigger on empty values"
        );
    }

    /// Error keys for a rich text `body` bounded to `min..=max` characters.
    fn richtext_length_keys(format: &str, min: usize, max: usize, value: Value) -> Vec<String> {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, body TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("body", FieldType::Richtext)
                .min_length(min)
                .max_length(max)
                .admin(FieldAdmin::builder().richtext_format(format).build())
                .build(),
        ];
        let data: DocumentFields = [("body".to_string(), value)].into_iter().collect();

        validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        )
        .err()
        .map(|e| e.errors.iter().filter_map(|fe| fe.key.clone()).collect())
        .unwrap_or_default()
    }

    /// Regression: rich text bounds counted the markup of an HTML value (and
    /// the serialization of a JSON one), and refused the document object form
    /// outright as "must be text". They measure the plain text now.
    #[test]
    fn richtext_bounds_measure_plain_text() {
        let html = json!("<p><strong>abc</strong></p>");
        assert!(richtext_length_keys("html", 1, 3, html.clone()).is_empty());
        assert_eq!(
            richtext_length_keys("html", 1, 2, html),
            vec!["validation.max_length"]
        );

        let doc = json!({ "type": "doc", "content": [
            { "type": "paragraph", "content": [{ "type": "text", "text": "abc" }] }
        ]});
        assert!(richtext_length_keys("json", 3, 3, doc.clone()).is_empty());
        assert!(richtext_length_keys("json", 3, 3, json!(doc.to_string())).is_empty());
        assert_eq!(
            richtext_length_keys("json", 4, 10, doc),
            vec!["validation.min_length"]
        );
    }

    /// Regression: an emptied editor's markup (`<p></p>`, an empty paragraph)
    /// was measured as zero characters, so `min_length` refused clearing an
    /// optional rich text field — bounds skip a blank value like an empty
    /// string.
    #[test]
    fn richtext_bounds_skip_a_blank_value() {
        assert!(richtext_length_keys("html", 3, 10, json!("<p></p>")).is_empty());

        let empty = json!({ "type": "doc", "content": [{ "type": "paragraph" }] });
        assert!(richtext_length_keys("json", 3, 10, empty).is_empty());
    }
}
