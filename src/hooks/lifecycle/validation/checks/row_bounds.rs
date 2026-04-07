use std::collections::HashMap;

use serde_json::Value;

use crate::core::{FieldDefinition, validate::FieldError};

/// Validate min_rows / max_rows for Array, Blocks, and has-many Relationship fields.
pub(crate) fn check_row_bounds(
    field: &FieldDefinition,
    data_key: &str,
    value: Option<&Value>,
    is_draft: bool,
    errors: &mut Vec<FieldError>,
) {
    if is_draft || (field.min_rows.is_none() && field.max_rows.is_none()) {
        return;
    }

    let row_count = match value {
        Some(Value::Array(arr)) => arr.len(),
        _ => 0,
    };

    if let Some(min) = field.min_rows
        && row_count < min
    {
        errors.push(FieldError::with_key(
            data_key.to_owned(),
            format!("{} requires at least {} item(s)", field.name, min),
            "validation.min_rows",
            HashMap::from([
                ("field".to_string(), field.name.clone()),
                ("min".to_string(), min.to_string()),
            ]),
        ));
    }

    if let Some(max) = field.max_rows
        && row_count > max
    {
        errors.push(FieldError::with_key(
            data_key.to_owned(),
            format!("{} allows at most {} item(s)", field.name, max),
            "validation.max_rows",
            HashMap::from([
                ("field".to_string(), field.name.clone()),
                ("max".to_string(), max.to_string()),
            ]),
        ));
    }
}

#[cfg(test)]
mod tests {
    use crate::core::field::{FieldDefinition, FieldType};
    use crate::hooks::lifecycle::validation::{ValidationCtx, validate_fields_inner};
    use serde_json::json;
    use std::collections::HashMap;

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
        let mut data = HashMap::new();
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
        let mut data = HashMap::new();
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
        let mut data = HashMap::new();
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
        let mut data = HashMap::new();
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
        let mut data = HashMap::new();
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
