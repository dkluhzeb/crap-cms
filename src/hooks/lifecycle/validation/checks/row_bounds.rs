use serde_json::Value;

use crate::core::{FieldDefinition, validate::FieldError};

/// Validate `min_rows` / `max_rows` for Array, Blocks, and has-many Relationship fields.
///
/// Scalar `has_many` fields (Text/Number/Select with a parent column) are
/// counted by `check_has_many_elements` instead — their JSON-string encoding
/// would count 0 here. Omitted fields on partial updates keep their stored
/// rows, mirroring the `check_required` exemption.
pub(crate) fn check_row_bounds(
    field: &FieldDefinition,
    data_key: &str,
    value: Option<&Value>,
    is_draft: bool,
    is_update: bool,
    errors: &mut Vec<FieldError>,
) {
    if is_draft || (field.min_rows.is_none() && field.max_rows.is_none()) {
        return;
    }

    if field.has_parent_column() && field.has_many {
        return;
    }

    if is_update && value.is_none() {
        return;
    }

    let row_count = match value {
        Some(Value::Array(arr)) => arr.len(),
        Some(Value::String(s)) => serde_json::from_str::<Vec<Value>>(s).map_or(0, |arr| arr.len()),
        _ => 0,
    };

    if let Some(min) = field.min_rows
        && row_count < min
    {
        errors.push(
            FieldError::with_key(
                data_key.to_owned(),
                format!("{} requires at least {} item(s)", field.name, min),
                "validation.min_rows",
            )
            .with_param("field", field.name.clone())
            .with_param("min", min.to_string()),
        );
    }

    if let Some(max) = field.max_rows
        && row_count > max
    {
        errors.push(
            FieldError::with_key(
                data_key.to_owned(),
                format!("{} allows at most {} item(s)", field.name, max),
                "validation.max_rows",
            )
            .with_param("field", field.name.clone())
            .with_param("max", max.to_string()),
        );
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use crate::core::DocumentFields;
    use crate::core::{FieldDefinition, FieldType};
    use crate::hooks::lifecycle::validation::{ValidationCtx, validate_fields_inner};
    use serde_json::json;

    #[test]
    fn test_validate_min_rows() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .min_rows(2)
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("items".to_string(), json!([{"label": "one"}]));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("at least 2"));
    }

    #[test]
    fn test_validate_max_rows() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .max_rows(1)
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("items".to_string(), json!([{"a": 1}, {"a": 2}]));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("at most 1"));
    }

    #[test]
    fn test_validate_min_rows_skipped_for_draft() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .min_rows(3)
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("items".to_string(), json!([{"x": 1}]));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").draft(true).build(),
        );
        assert!(
            result.is_ok(),
            "min_rows should not be checked for draft saves"
        );
    }

    #[test]
    fn test_validate_max_rows_skipped_for_draft() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .max_rows(1)
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("items".to_string(), json!([{"a": 1}, {"a": 2}, {"a": 3}]));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").draft(true).build(),
        );
        assert!(
            result.is_ok(),
            "max_rows should not be checked for draft saves"
        );
    }

    /// Regression: a scalar `has_many` field stores its values as a JSON
    /// string (admin-form encoding). `check_row_bounds` counted only real
    /// arrays, so `min_rows` was unsatisfiable — every save failed even
    /// with enough values. Counting is delegated to
    /// `check_has_many_elements`, which understands the encoding.
    #[test]
    fn test_min_rows_satisfied_by_has_many_json_string() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .min_rows(2)
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
            result.is_ok(),
            "3 has_many values must satisfy min_rows=2, got: {result:?}"
        );
    }

    /// Regression: a partial update that omits an array field falsely
    /// failed `min_rows` (None counted as 0 rows). Omitted fields keep
    /// their stored rows — mirror the `check_required` update exemption.
    #[test]
    fn test_min_rows_skipped_for_omitted_field_on_update() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("gallery", FieldType::Array)
                .min_rows(1)
                .build(),
        ];
        let data = DocumentFields::new();
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test")
                .exclude_id(Some("doc-1"))
                .build(),
        );
        assert!(
            result.is_ok(),
            "omitted array on update must not fail min_rows, got: {result:?}"
        );
    }

    /// The create side stays fail-closed: an omitted array with
    /// `min_rows >= 1` still fails on create.
    #[test]
    fn test_min_rows_still_enforced_for_omitted_field_on_create() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("gallery", FieldType::Array)
                .min_rows(1)
                .build(),
        ];
        let data = DocumentFields::new();
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "omitted array on create must fail min_rows"
        );
    }

    #[test]
    fn test_validate_min_rows_non_array_value_treated_as_zero() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .min_rows(1)
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("items".to_string(), json!("not-an-array"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "Non-array value with min_rows=1 should fail (count=0)"
        );
        assert!(result.unwrap_err().errors[0].message.contains("at least 1"));
    }
}
