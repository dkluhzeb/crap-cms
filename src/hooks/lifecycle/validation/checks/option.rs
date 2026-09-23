use serde_json::Value;

use crate::core::{FieldDefinition, FieldType, validate::FieldError};

use super::shared::{decode_element_list, element_display};

/// Whether the field still declares `value` as one of its options. A field
/// that declares no options at all accepts any text — its values are still
/// shape-checked (text, or a list of text for `has_many`).
fn declares(field: &FieldDefinition, value: &str) -> bool {
    field.options.is_empty() || field.options.iter().any(|opt| opt.value == value)
}

/// Whether a submitted value passes: one the field still declares, or one the
/// stored document already holds.
///
/// Retiring an option must not make every later save of a document that carries
/// it fail — an editor fixing an unrelated typo cannot be asked to pick a new
/// one. A value the document does not already hold is still rejected, so a
/// retired option can never be newly chosen.
fn accepted(field: &FieldDefinition, held: &[String], value: &str) -> bool {
    declares(field, value) || held.iter().any(|h| h == value)
}

/// Every value a select/radio submission carries, whatever its encoding: the
/// whole list for a `has_many` field, the single value otherwise.
fn submitted_values(field: &FieldDefinition, value: Option<&Value>) -> Vec<Value> {
    if field.has_many {
        return decode_element_list(value).unwrap_or_default();
    }

    value.into_iter().cloned().collect()
}

/// The values a select/radio submission carries that the field no longer
/// declares — empty when every value is still declared, which is the common
/// case. Lets a caller decide whether reading what the document already holds
/// is worth a query at all.
pub(crate) fn undeclared_values(field: &FieldDefinition, value: Option<&Value>) -> Vec<String> {
    if field.field_type != FieldType::Select && field.field_type != FieldType::Radio {
        return Vec::new();
    }

    submitted_values(field, value)
        .iter()
        .filter_map(Value::as_str)
        .filter(|s| !declares(field, s))
        .map(ToString::to_string)
        .collect()
}

/// The values a stored select/radio column holds: its whole list for a
/// `has_many` field, the single stored value otherwise. A null or
/// wrongly-shaped stored value holds nothing.
pub(crate) fn held_values(field: &FieldDefinition, stored: &Value) -> Vec<String> {
    if field.has_many {
        return decode_element_list(Some(stored))
            .unwrap_or_default()
            .iter()
            .filter_map(Value::as_str)
            .map(ToString::to_string)
            .collect();
    }

    stored
        .as_str()
        .map(ToString::to_string)
        .into_iter()
        .collect()
}

/// A select/radio value under validation.
///
/// [`held`](Self::held) attaches the values the stored document already carries
/// for the field; without it the value is judged on the declared options alone,
/// which is what a create wants and all a value with no column of its own (a
/// sub-field inside an array/blocks row) can be judged on.
pub(crate) struct OptionCheck<'a> {
    field: &'a FieldDefinition,
    data_key: &'a str,
    value: Option<&'a Value>,
    is_empty: bool,
    held: &'a [String],
}

impl<'a> OptionCheck<'a> {
    pub(crate) fn new(
        field: &'a FieldDefinition,
        data_key: &'a str,
        value: Option<&'a Value>,
        is_empty: bool,
    ) -> Self {
        Self {
            field,
            data_key,
            value,
            is_empty,
            held: &[],
        }
    }

    /// The values the stored row already holds for this field.
    #[must_use]
    pub(crate) fn held(mut self, held: &'a [String]) -> Self {
        self.held = held;

        self
    }
}

/// Validate that Select/Radio value exists in the options list. A present
/// value of the wrong shape (non-string single value, undecodable `has_many`
/// list) is an invalid option — never a silent pass.
pub(crate) fn check_option_valid(check: &OptionCheck<'_>, errors: &mut Vec<FieldError>) {
    let field = check.field;

    if (field.field_type != FieldType::Select && field.field_type != FieldType::Radio)
        || check.is_empty
    {
        return;
    }

    if field.has_many {
        check_has_many_options(check, errors);
        return;
    }

    let valid = matches!(
        check.value,
        Some(Value::String(s)) if accepted(field, check.held, s)
    );
    if !valid {
        errors.push(
            FieldError::with_key(
                check.data_key.to_owned(),
                format!("{} has an invalid option", field.name),
                "validation.invalid_option",
            )
            .with_param("field", field.name.clone()),
        );
    }
}

/// Validate each value in a `has_many` select/radio element list against the
/// options list. Accepts both the typed array and JSON-string encodings.
fn check_has_many_options(check: &OptionCheck<'_>, errors: &mut Vec<FieldError>) {
    let field = check.field;

    let Some(values) = decode_element_list(check.value) else {
        errors.push(
            FieldError::with_key(
                check.data_key.to_owned(),
                format!(
                    "{} has invalid multi-select value (malformed JSON)",
                    field.name
                ),
                "validation.invalid_multi_select_json",
            )
            .with_param("field", field.name.clone()),
        );

        return;
    };

    for v in &values {
        let valid = v.as_str().is_some_and(|s| accepted(field, check.held, s));

        if !valid {
            errors.push(
                FieldError::with_key(
                    check.data_key.to_owned(),
                    format!(
                        "{} has an invalid option: {}",
                        field.name,
                        element_display(v)
                    ),
                    "validation.invalid_option_value",
                )
                .with_param("field", field.name.clone())
                .with_param("value", element_display(v)),
            );
        }
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use crate::core::DocumentFields;
    use crate::core::validate::ValidationError;
    use crate::core::{FieldDefinition, FieldType, LocalizedString, SelectOption};
    use crate::hooks::lifecycle::validation::{ValidationCtx, validate_fields_inner};
    use serde_json::{Value, json};

    fn choice(name: &str, has_many: bool, values: &[&str]) -> FieldDefinition {
        let options = values
            .iter()
            .map(|v| SelectOption::new(LocalizedString::Plain(v.to_uppercase()), *v))
            .collect();

        FieldDefinition::builder(name, FieldType::Select)
            .has_many(has_many)
            .options(options)
            .build()
    }

    fn error_keys(result: &Result<(), ValidationError>) -> Vec<String> {
        result.as_ref().err().map_or_else(Vec::new, |err| {
            err.errors.iter().filter_map(|e| e.key.clone()).collect()
        })
    }

    /// Regression: a stored value the field no longer declares blocked EVERY
    /// later save of the document that holds it — an editor fixing an unrelated
    /// typo was told the untouched select had "an invalid option". The value the
    /// row already carries is accepted on update; a newly chosen undeclared one
    /// is not, and a create has no stored value to fall back on.
    #[test]
    fn a_retired_option_the_row_already_holds_survives_an_unrelated_edit() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE test (id TEXT PRIMARY KEY, status TEXT, title TEXT);
             INSERT INTO test (id, status, title) VALUES ('doc1', 'legacy', 'old');",
        )
        .unwrap();

        // `legacy` is no longer offered; the row still holds it.
        let fields = vec![
            choice("status", false, &["draft", "published"]),
            FieldDefinition::builder("title", FieldType::Text).build(),
        ];

        let mut data = DocumentFields::new();
        data.insert("status".to_string(), json!("legacy"));
        data.insert("title".to_string(), json!("fixed typo"));

        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test")
                .exclude_id(Some("doc1"))
                .build(),
        );

        assert!(
            result.is_ok(),
            "the value the row already holds must not block the edit: {:?}",
            error_keys(&result)
        );
    }

    /// Switching to a different undeclared value is still rejected — the rescue
    /// is for what the document already carries, not a free pass.
    #[test]
    fn a_different_undeclared_value_is_still_rejected_on_update() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE test (id TEXT PRIMARY KEY, status TEXT);
             INSERT INTO test (id, status) VALUES ('doc1', 'legacy');",
        )
        .unwrap();

        let fields = vec![choice("status", false, &["draft", "published"])];

        let mut data = DocumentFields::new();
        data.insert("status".to_string(), json!("invented"));

        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test")
                .exclude_id(Some("doc1"))
                .build(),
        );

        assert!(
            error_keys(&result).contains(&"validation.invalid_option".to_string()),
            "a value the row does not hold must still be rejected: {:?}",
            error_keys(&result)
        );
    }

    /// A create has no stored value at all, so an undeclared value is rejected
    /// exactly as before.
    #[test]
    fn an_undeclared_value_is_rejected_on_create() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, status TEXT)")
            .unwrap();

        let fields = vec![choice("status", false, &["draft", "published"])];

        let mut data = DocumentFields::new();
        data.insert("status".to_string(), json!("legacy"));

        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );

        assert!(
            error_keys(&result).contains(&"validation.invalid_option".to_string()),
            "create has no stored value to fall back on: {:?}",
            error_keys(&result)
        );
    }

    /// The same rule per element for a `has_many` picker: the retired value the
    /// row already holds passes, a newly added undeclared one does not.
    #[test]
    fn a_has_many_picker_rescues_only_the_elements_the_row_holds() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE test (id TEXT PRIMARY KEY, colors TEXT);
             INSERT INTO test (id, colors) VALUES ('doc1', '[\"red\",\"puce\"]');",
        )
        .unwrap();

        let fields = vec![choice("colors", true, &["red", "blue"])];

        let mut kept = DocumentFields::new();
        kept.insert("colors".to_string(), json!(["red", "puce"]));
        let kept_result = validate_fields_inner(
            &lua,
            &fields,
            &kept,
            &ValidationCtx::builder(&conn, "test")
                .exclude_id(Some("doc1"))
                .build(),
        );
        assert!(
            kept_result.is_ok(),
            "the retired element the row holds survives: {:?}",
            error_keys(&kept_result)
        );

        let mut added = DocumentFields::new();
        added.insert("colors".to_string(), json!(["red", "puce", "chartreuse"]));
        let added_result = validate_fields_inner(
            &lua,
            &fields,
            &added,
            &ValidationCtx::builder(&conn, "test")
                .exclude_id(Some("doc1"))
                .build(),
        );
        assert!(
            error_keys(&added_result).contains(&"validation.invalid_option_value".to_string()),
            "a newly added undeclared element is still rejected: {:?}",
            error_keys(&added_result)
        );
    }

    /// Regression: a `has_many` select submitted as a typed array (Lua/gRPC)
    /// bypassed the options allowlist entirely — only the JSON-string
    /// encoding was checked.
    #[test]
    fn has_many_select_typed_array_invalid_option_rejected() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, colors TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("colors", FieldType::Select)
                .has_many(true)
                .options(vec![
                    SelectOption::new(LocalizedString::Plain("Red".to_string()), "red"),
                    SelectOption::new(LocalizedString::Plain("Blue".to_string()), "blue"),
                ])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("colors".to_string(), json!(["red", "green"]));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err(), "'green' is not an allowed option");
        let err = result.unwrap_err();
        assert!(
            err.errors
                .iter()
                .any(|e| e.key.as_deref() == Some("validation.invalid_option_value")),
            "expected invalid_option_value, got: {:?}",
            err.errors
        );
    }

    /// Regression: a present non-string value on a single Select/Radio
    /// silently bypassed the allowlist (e.g. `status = 5` persisted).
    #[test]
    fn single_select_non_string_value_rejected() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, color TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("color", FieldType::Select)
                .options(vec![SelectOption::new(
                    LocalizedString::Plain("Red".to_string()),
                    "red",
                )])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("color".to_string(), json!(5));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err(), "non-string select value must be rejected");
        let err = result.unwrap_err();
        assert!(
            err.errors
                .iter()
                .any(|e| e.key.as_deref() == Some("validation.invalid_option")),
            "expected invalid_option, got: {:?}",
            err.errors
        );
    }

    #[test]
    fn test_validate_select_option_valid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, color TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("color", FieldType::Select)
                .options(vec![
                    SelectOption::new(LocalizedString::Plain("Red".to_string()), "red"),
                    SelectOption::new(LocalizedString::Plain("Blue".to_string()), "blue"),
                ])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("color".to_string(), json!("red"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_select_option_invalid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, color TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("color", FieldType::Select)
                .options(vec![SelectOption::new(
                    LocalizedString::Plain("Red".to_string()),
                    "red",
                )])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("color".to_string(), json!("green"));
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
                .contains("invalid option")
        );
    }

    #[test]
    fn test_validate_select_option_empty_value_passes() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, color TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("color", FieldType::Select)
                .options(vec![SelectOption::new(
                    LocalizedString::Plain("Red".to_string()),
                    "red",
                )])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("color".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_ok(),
            "Empty select value should pass (not required)"
        );
    }

    #[test]
    fn test_validate_radio_option_valid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, size TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("size", FieldType::Radio)
                .options(vec![
                    SelectOption::new(LocalizedString::Plain("Small".to_string()), "sm"),
                    SelectOption::new(LocalizedString::Plain("Large".to_string()), "lg"),
                ])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("size".to_string(), json!("sm"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok(), "Valid radio option should pass");
    }

    #[test]
    fn test_validate_radio_option_invalid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, size TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("size", FieldType::Radio)
                .options(vec![SelectOption::new(
                    LocalizedString::Plain("Small".to_string()),
                    "sm",
                )])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("size".to_string(), json!("xl"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err(), "Invalid radio option should fail");
        assert!(
            result.unwrap_err().errors[0]
                .message
                .contains("invalid option")
        );
    }

    #[test]
    fn test_validate_radio_option_empty_passes() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, size TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("size", FieldType::Radio)
                .options(vec![SelectOption::new(
                    LocalizedString::Plain("Small".to_string()),
                    "sm",
                )])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("size".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_ok(),
            "Empty radio value should skip option validation"
        );
    }

    /// Regression: malformed JSON in a `has_many` select must produce a
    /// validation error, not silently pass.
    #[test]
    fn test_validate_has_many_select_malformed_json_rejected() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Select)
                .has_many(true)
                .options(vec![
                    SelectOption::new(LocalizedString::Plain("A".to_string()), "a"),
                    SelectOption::new(LocalizedString::Plain("B".to_string()), "b"),
                ])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("tags".to_string(), json!("[invalid json"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err(), "Malformed has_many JSON must be rejected");
        assert!(
            result.unwrap_err().errors[0]
                .message
                .contains("malformed JSON"),
        );
    }

    /// Regression: `has_many` select must report ALL invalid options, not just the first.
    /// Previously, `break` after the first error caused subsequent violations to be hidden.
    #[test]
    fn test_has_many_select_reports_all_invalid_options() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Select)
                .has_many(true)
                .options(vec![SelectOption::new(
                    LocalizedString::Plain("Red".to_string()),
                    "red",
                )])
                .build(),
        ];
        let mut data = DocumentFields::new();
        // Two invalid options: "invalid1" and "invalid2"
        data.insert(
            "tags".to_string(),
            json!(r#"["invalid1","red","invalid2"]"#),
        );
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        let errors = &result.unwrap_err().errors;
        assert_eq!(
            errors.len(),
            2,
            "Both invalid options should produce errors, got {}",
            errors.len()
        );
    }

    #[test]
    fn test_validate_select_no_options_skips_option_check() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, status TEXT)")
            .unwrap();
        let fields = vec![FieldDefinition::builder("status", FieldType::Select).build()];
        let mut data = DocumentFields::new();
        data.insert("status".to_string(), json!("anything"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_ok(),
            "Select with no options should not validate option values"
        );
    }

    /// Regression: a select/radio with no options accepted any JSON type —
    /// a number, an object — which was stored as its JSON spelling.
    #[test]
    fn a_field_without_options_still_rejects_non_text_values() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, one TEXT, many TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("one", FieldType::Select).build(),
            choice("many", true, &[]),
        ];

        let validate = |one: Value, many: Value| {
            let mut data = DocumentFields::new();
            data.insert("one".to_string(), one);
            data.insert("many".to_string(), many);
            validate_fields_inner(
                &lua,
                &fields,
                &data,
                &ValidationCtx::builder(&conn, "test").build(),
            )
        };

        assert!(validate(json!("x"), json!(["a", "b"])).is_ok());
        assert_eq!(
            error_keys(&validate(json!(42), json!(["a", 7]))),
            vec![
                "validation.invalid_option",
                "validation.invalid_option_value"
            ]
        );
        assert_eq!(
            error_keys(&validate(json!("x"), json!({"a": 1}))),
            vec!["validation.invalid_multi_select_json"]
        );
    }
}
