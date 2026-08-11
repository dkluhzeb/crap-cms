use serde_json::Value;

use crate::core::{FieldDefinition, FieldType, validate::FieldError};
use crate::hooks::lifecycle::validation::runner::is_empty_value;

/// Check required constraint. For Array and has-many Relationship, "required"
/// means at least one item.
///
/// `skip` collapses the caller's reasons to bypass this per-submit check: the
/// document is a draft, OR the field is **locale-scoped** while localization is
/// active. Locale-scoped required fields are instead enforced document-wide by
/// the completeness check (`check_localized_completeness`) against the field's
/// effective `required_locales` — so this per-submit check only enforces
/// `required` for non-localized fields (and for every field when localization
/// is disabled). Checkboxes and absent fields on update are also skipped.
pub(crate) fn check_required(
    field: &FieldDefinition,
    data_key: &str,
    value: Option<&Value>,
    required: bool,
    skip: bool,
    is_update: bool,
    errors: &mut Vec<FieldError>,
) {
    if !required
        || skip
        || field.field_type == FieldType::Checkbox
        || (is_update && value.is_none())
    {
        return;
    }

    if is_value_present(field, value, is_empty_value(value)) {
        return;
    }

    errors.push(
        FieldError::with_key(
            data_key.to_owned(),
            format!("{} is required", field.name),
            "validation.required",
        )
        .with_param("field", field.name.clone()),
    );
}

/// Check if a field value is "present" for required validation purposes.
/// Join-shaped and `has_many` fields accept both value encodings that reach
/// validation: the typed `Value::Array` (Lua/gRPC) and the JSON-string
/// encoding (admin form).
fn is_value_present(field: &FieldDefinition, value: Option<&Value>, is_empty: bool) -> bool {
    if !field.has_parent_column() {
        // Array/Blocks/Relationship (join-shaped)
        return match value {
            Some(Value::Array(arr)) => !arr.is_empty(),
            Some(Value::String(s)) => match field.field_type {
                // A bare string is meaningless for Array/Blocks — only a
                // non-empty JSON-encoded array counts as present.
                FieldType::Array | FieldType::Blocks => {
                    serde_json::from_str::<Vec<Value>>(s).is_ok_and(|arr| !arr.is_empty())
                }
                // has-many relationship: JSON-array string, or a bare id
                _ if field.has_many => serde_json::from_str::<Vec<Value>>(s)
                    .map_or(!s.is_empty(), |arr| !arr.is_empty()),
                // has-one relationship/upload: a non-empty id string
                _ => !s.is_empty(),
            },
            _ => false,
        };
    }

    if field.has_many {
        // has_many with parent column: typed array or JSON array string
        return match value {
            Some(Value::Array(arr)) => !arr.is_empty(),
            Some(Value::String(s)) => {
                serde_json::from_str::<Vec<Value>>(s).map_or(!s.is_empty(), |arr| !arr.is_empty())
            }
            _ => false,
        };
    }

    !is_empty
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::core::DocumentFields;
    use crate::core::RelationshipConfig;
    use crate::hooks::lifecycle::validation::{ValidationCtx, validate_fields_inner};
    use serde_json::json;

    /// Regression: a `has_many` scalar field submitted as a typed array
    /// (Lua/gRPC) falsely failed `required` — the presence check only
    /// understood the JSON-string encoding.
    #[test]
    fn has_many_required_satisfied_by_typed_array() {
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
        data.insert("tags".to_string(), json!(["x"]));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_ok(),
            "non-empty typed array must satisfy required, got: {result:?}"
        );
    }

    /// Regression: a bare non-empty string satisfied `required` on an
    /// Array/Blocks field even though nothing would be persisted to the
    /// join table — silent data loss on a required field.
    #[test]
    fn array_required_not_satisfied_by_bare_string() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .required(true)
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("items".to_string(), json!("hello"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "a bare string must not satisfy required on an Array field"
        );
    }

    #[test]
    fn test_validate_required_field_empty_string() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("name", FieldType::Text)
                .required(true)
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
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.errors.len(), 1);
        assert!(err.errors[0].message.contains("required"));
    }

    #[test]
    fn test_validate_required_field_null() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("name", FieldType::Text)
                .required(true)
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("name".to_string(), json!(null));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_required_skipped_for_drafts() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("name", FieldType::Text)
                .required(true)
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("name".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").draft(true).build(),
        );
        assert!(result.is_ok(), "Drafts should skip required checks");
    }

    #[test]
    fn test_validate_required_join_field_empty_array() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Relationship)
                .required(true)
                .relationship(RelationshipConfig::new("tags", true))
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("tags".to_string(), json!([]));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("required"));
    }

    #[test]
    fn test_validate_required_join_field_non_empty_array() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Relationship)
                .required(true)
                .relationship(RelationshipConfig::new("tags", true))
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("tags".to_string(), json!(["t1", "t2"]));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_required_skipped_on_update_absent_field() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("name", FieldType::Text)
                .required(true)
                .build(),
        ];
        let data = DocumentFields::new();
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test")
                .exclude_id(Some("p1"))
                .build(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_checkbox_not_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, active INTEGER)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("active", FieldType::Checkbox)
                .required(true)
                .build(),
        ];
        let data = DocumentFields::new();
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_required_array_field_empty_array() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .required(true)
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("items".to_string(), json!([]));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "Empty array for required array field should fail"
        );
        assert!(result.unwrap_err().errors[0].message.contains("required"));
    }

    #[test]
    fn test_validate_required_array_field_non_empty_passes() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .required(true)
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("items".to_string(), json!([{"x": 1}]));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_ok(),
            "Non-empty array for required array field should pass"
        );
    }
}
