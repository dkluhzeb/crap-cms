//! `has_many` scalar list validation: the list's shape and its
//! `min_rows`/`max_rows` count, with each element checked by the `elements` submodule.

mod elements;

use serde_json::Value;

use crate::core::{FieldDefinition, FieldType, validate::FieldError};

use super::shared::decode_element_list;
use elements::{check_number_value_bounds, check_text_element};

/// The inputs of [`check_has_many_elements`].
pub(crate) struct HasManyCheck<'a> {
    field: &'a FieldDefinition,
    data_key: &'a str,
    value: Option<&'a Value>,
    is_empty: bool,
    is_draft: bool,
    is_update: bool,
}

impl<'a> HasManyCheck<'a> {
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
            is_draft: false,
            is_update: false,
        }
    }

    /// A draft save relaxes the `min_rows`/`max_rows` count.
    #[must_use]
    pub(crate) fn draft(mut self, is_draft: bool) -> Self {
        self.is_draft = is_draft;

        self
    }

    /// On an update an omitted field keeps its stored values, so its count is
    /// not judged. A value inside a row or a node is always sent whole.
    #[must_use]
    pub(crate) fn update(mut self, is_update: bool) -> Self {
        self.is_update = is_update;

        self
    }
}

/// Validate individual values within a `has_many` element list.
/// Checks count bounds (`min_rows/max_rows`) for all `has_many` field types
/// and per-element constraints for Text/Number. Accepts both the typed
/// `Value::Array` shape (Lua/gRPC) and the JSON-string encoding (admin form).
///
/// An absent, null or empty value counts as zero values — the rule
/// `check_row_bounds` applies to Array/Blocks/has-many relationships — except
/// on an update that omits the field, which keeps what is stored.
pub(crate) fn check_has_many_elements(check: &HasManyCheck<'_>, errors: &mut Vec<FieldError>) {
    let HasManyCheck {
        field,
        data_key,
        value,
        is_empty,
        is_draft,
        is_update,
    } = *check;

    let relevant = matches!(
        field.field_type,
        FieldType::Select | FieldType::Radio | FieldType::Text | FieldType::Number
    );
    if !field.has_many || !relevant {
        return;
    }

    if is_empty {
        if !is_draft && (!is_update || value.is_some()) {
            check_count_bounds(field, data_key, 0, errors);
        }
        return;
    }

    let Some(values) = decode_element_list(value) else {
        // Select/Radio report a malformed list via `check_has_many_options`;
        // for Text/Number this is the only validator, so reject here (mirroring
        // it) instead of silently coercing the value to an empty list — which
        // would drop the submitted value and bypass any `min_rows`/`min_length`.
        if matches!(field.field_type, FieldType::Text | FieldType::Number) {
            errors.push(
                FieldError::with_key(
                    data_key.to_owned(),
                    format!("{} must be a list", field.name),
                    "validation.invalid_has_many_json",
                )
                .with_param("field", field.name.clone()),
            );
        }

        return;
    };

    // `min_rows`/`max_rows` are relaxed on draft saves — same as
    // `check_row_bounds` does for Array/Blocks/has-many-relationship. Per-element
    // value bounds below still run on drafts (only the count rule is relaxed).
    if !is_draft {
        check_count_bounds(field, data_key, values.len(), errors);
    }

    // Select/Radio: per-element option validation is in check_option_valid.
    if field.field_type == FieldType::Select || field.field_type == FieldType::Radio {
        return;
    }

    for v in &values {
        match field.field_type {
            FieldType::Text => check_text_element(field, data_key, v, errors),
            FieldType::Number => check_number_value_bounds(field, data_key, v, errors),
            _ => {}
        }
    }
}

/// Shared `min_rows/max_rows` validation for all `has_many` field types.
fn check_count_bounds(
    field: &FieldDefinition,
    data_key: &str,
    count: usize,
    errors: &mut Vec<FieldError>,
) {
    if let Some(min_rows) = field.min_rows
        && count < min_rows
    {
        errors.push(
            FieldError::with_key(
                data_key.to_owned(),
                format!("{} must have at least {} values", field.name, min_rows),
                "validation.has_many_min_rows",
            )
            .with_param("field", field.name.clone())
            .with_param("min", min_rows.to_string()),
        );
    }

    if let Some(max_rows) = field.max_rows
        && count > max_rows
    {
        errors.push(
            FieldError::with_key(
                data_key.to_owned(),
                format!("{} must have at most {} values", field.name, max_rows),
                "validation.has_many_max_rows",
            )
            .with_param("field", field.name.clone())
            .with_param("max", max_rows.to_string()),
        );
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use crate::core::DocumentFields;
    use crate::core::{FieldDefinition, FieldType, LocalizedString, SelectOption};
    use crate::hooks::lifecycle::validation::{
        ValidationCtx, is_empty_value, validate_fields_inner,
    };

    use super::{HasManyCheck, check_has_many_elements};
    use serde_json::{Value, json};

    fn min_rows_errors(value: Option<&Value>, is_update: bool) -> usize {
        let field = FieldDefinition::builder("tags", FieldType::Text)
            .has_many(true)
            .min_rows(1)
            .build();
        let is_empty = is_empty_value(value);
        let mut errors = Vec::new();

        check_has_many_elements(
            &HasManyCheck::new(&field, "tags", value, is_empty).update(is_update),
            &mut errors,
        );

        errors.len()
    }

    /// Regression: `min_rows` on a scalar has-many list was skipped when the
    /// value was absent, null or empty — Array/Blocks/relationship lists count
    /// those as zero. An update that omits the field keeps its stored values.
    #[test]
    fn min_rows_counts_an_absent_or_blank_list_as_zero() {
        assert_eq!(min_rows_errors(None, false), 1, "absent on create");
        assert_eq!(min_rows_errors(Some(&Value::Null), false), 1);
        assert_eq!(
            min_rows_errors(Some(&json!("")), true),
            1,
            "cleared on update"
        );
        assert_eq!(
            min_rows_errors(None, true),
            0,
            "omitted on update keeps the stored list"
        );
        assert_eq!(min_rows_errors(Some(&json!(["a"])), false), 0);
    }

    /// Regression: a malformed (scalar / bare-string) value on a has-many
    /// Text/Number field was silently coerced to an empty list — dropping the
    /// submitted value and bypassing `min_rows`. Reject it, mirroring how
    /// has-many Select/Radio reject the same shape.
    #[test]
    fn malformed_has_many_scalar_value_rejected() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .build(),
        ];
        // A bare scalar, not a list or a JSON-array string.
        let mut data = DocumentFields::new();
        data.insert("tags".to_string(), json!(5));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "a malformed has-many scalar must be rejected, not coerced to []"
        );
        assert!(
            result
                .unwrap_err()
                .errors
                .iter()
                .any(|e| e.key.as_deref() == Some("validation.invalid_has_many_json"))
        );
    }

    /// Regression companion: count bounds must also fire for typed arrays.
    #[test]
    fn typed_array_count_bounds_enforced() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .min_rows(2)
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("tags".to_string(), json!(["only-one"]));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err(), "1 element violates min_rows=2");
        let err = result.unwrap_err();
        assert!(
            err.errors
                .iter()
                .any(|e| e.key.as_deref() == Some("validation.has_many_min_rows")),
            "expected has_many_min_rows, got: {:?}",
            err.errors
        );
    }

    /// Regression: `min_rows`/`max_rows` on a scalar `has_many` field were
    /// enforced even on draft saves, while Array/Blocks/relationship count
    /// bounds are relaxed for drafts (`check_row_bounds` skips on draft). The
    /// two are now symmetric.
    #[test]
    fn has_many_count_bounds_skipped_on_draft() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .min_rows(3)
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("tags".to_string(), json!(["one"]));

        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").draft(true).build(),
        );
        assert!(
            result.is_ok(),
            "draft save must relax min_rows for scalar has_many, got: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_validate_has_many_select_valid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Select)
                .has_many(true)
                .options(vec![
                    SelectOption::new(LocalizedString::Plain("Red".to_string()), "red"),
                    SelectOption::new(LocalizedString::Plain("Blue".to_string()), "blue"),
                ])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("tags".to_string(), json!(r#"["red","blue"]"#));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok(), "Valid has_many select values should pass");
    }

    #[test]
    fn test_validate_has_many_select_invalid_option() {
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
        data.insert("tags".to_string(), json!(r#"["red","invalid"]"#));
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
    fn test_validate_has_many_select_empty_array() {
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
        data.insert("tags".to_string(), json!("[]"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_ok(),
            "Empty array for has_many select should pass"
        );
    }

    #[test]
    fn test_validate_has_many_text_max_rows_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .max_rows(2)
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("tags".to_string(), json!(r#"["a","b","c"]"#));
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
                .contains("at most 2 values")
        );
    }

    #[test]
    fn test_has_many_text_required_empty_array_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .required(true)
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("tags".to_string(), json!("[]"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err(), "Empty array should fail required check");
        assert!(result.unwrap_err().errors[0].message.contains("required"));
    }

    #[test]
    fn test_has_many_text_required_with_values_passes() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .required(true)
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("tags".to_string(), json!(r#"["rust"]"#));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok(), "Non-empty array should pass required check");
    }

    #[test]
    fn test_validate_has_many_text_min_rows_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .min_rows(3)
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("tags".to_string(), json!(r#"["a","b"]"#));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "has_many text with fewer items than min_rows should fail"
        );
        assert!(result.unwrap_err().errors[0].message.contains("at least 3"));
    }

    #[test]
    fn test_validate_has_many_number_min_rows_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, scores TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("scores", FieldType::Number)
                .has_many(true)
                .min_rows(2)
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("scores".to_string(), json!(r#"["10"]"#));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "has_many number with fewer items than min_rows should fail"
        );
        assert!(result.unwrap_err().errors[0].message.contains("at least 2"));
    }

    /// Regression: `has_many` Select must enforce `min_rows/max_rows` bounds.
    /// Previously, `check_has_many_elements` only handled Text/Number, so
    /// Select/Radio `has_many` fields silently bypassed row count validation.
    #[test]
    fn test_has_many_select_min_rows_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Select)
                .has_many(true)
                .min_rows(2)
                .options(vec![
                    SelectOption::new(LocalizedString::Plain("A".to_string()), "a"),
                    SelectOption::new(LocalizedString::Plain("B".to_string()), "b"),
                    SelectOption::new(LocalizedString::Plain("C".to_string()), "c"),
                ])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("tags".to_string(), json!(r#"["a"]"#));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "has_many select with 1 value should fail min_rows=2"
        );
        assert!(result.unwrap_err().errors[0].message.contains("at least 2"));
    }

    /// Regression: `has_many` Select must enforce `max_rows` bounds.
    #[test]
    fn test_has_many_select_max_rows_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Select)
                .has_many(true)
                .max_rows(2)
                .options(vec![
                    SelectOption::new(LocalizedString::Plain("A".to_string()), "a"),
                    SelectOption::new(LocalizedString::Plain("B".to_string()), "b"),
                    SelectOption::new(LocalizedString::Plain("C".to_string()), "c"),
                ])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("tags".to_string(), json!(r#"["a","b","c"]"#));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "has_many select with 3 values should fail max_rows=2"
        );
        assert!(result.unwrap_err().errors[0].message.contains("at most 2"));
    }

    /// Regression: `has_many` Radio must enforce `min_rows` bounds.
    #[test]
    fn test_has_many_radio_min_rows_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, sizes TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("sizes", FieldType::Radio)
                .has_many(true)
                .min_rows(2)
                .options(vec![
                    SelectOption::new(LocalizedString::Plain("S".to_string()), "s"),
                    SelectOption::new(LocalizedString::Plain("M".to_string()), "m"),
                    SelectOption::new(LocalizedString::Plain("L".to_string()), "l"),
                ])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("sizes".to_string(), json!(r#"["s"]"#));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "has_many radio with 1 value should fail min_rows=2"
        );
        assert!(result.unwrap_err().errors[0].message.contains("at least 2"));
    }
}
