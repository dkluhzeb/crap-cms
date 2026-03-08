use crate::core::field::FieldDefinition;
use crate::core::validate::FieldError;
use crate::db::query;

/// Check unique constraint (only if value is non-empty and field has a parent column).
pub(crate) fn check_unique(
    field: &FieldDefinition,
    data_key: &str,
    value: Option<&serde_json::Value>,
    is_empty: bool,
    conn: &rusqlite::Connection,
    table: &str,
    exclude_id: Option<&str>,
    errors: &mut Vec<FieldError>,
) {
    if !field.unique || is_empty || !field.has_parent_column() {
        return;
    }
    let value_str = match value {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    };
    match query::count_where_field_eq(conn, table, data_key, &value_str, exclude_id) {
        Ok(count) if count > 0 => {
            errors.push(FieldError::new(data_key.to_owned(), format!("{} must be unique", field.name)));
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!("Unique check failed for {}.{}: {}", table, data_key, e);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::core::field::{FieldDefinition, FieldType};
    use crate::hooks::lifecycle::validation::validate_fields_inner;
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn test_validate_unique_check() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE test (id TEXT PRIMARY KEY, email TEXT);
             INSERT INTO test (id, email) VALUES ('existing', 'taken@test.com');"
        ).unwrap();
        let fields = vec![FieldDefinition {
            name: "email".to_string(),
            unique: true,
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("email".to_string(), json!("taken@test.com"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("unique"));
    }

    #[test]
    fn test_validate_unique_check_excludes_self() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE test (id TEXT PRIMARY KEY, email TEXT);
             INSERT INTO test (id, email) VALUES ('self', 'me@test.com');"
        ).unwrap();
        let fields = vec![FieldDefinition {
            name: "email".to_string(),
            unique: true,
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("email".to_string(), json!("me@test.com"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", Some("self"), false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_unique_check_skips_empty_value() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE test (id TEXT PRIMARY KEY, slug TEXT);
             INSERT INTO test (id, slug) VALUES ('a', '');"
        ).unwrap();
        let fields = vec![FieldDefinition {
            name: "slug".to_string(),
            unique: true,
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("slug".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok(), "Unique check should not fire on empty value");
    }

    #[test]
    fn test_validate_unique_check_with_number_value() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE test (id TEXT PRIMARY KEY, rank REAL);
             INSERT INTO test (id, rank) VALUES ('existing', 42);"
        ).unwrap();
        let fields = vec![FieldDefinition {
            name: "rank".to_string(),
            field_type: FieldType::Number,
            unique: true,
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("rank".to_string(), json!(42));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err(), "Duplicate number value should fail unique check");
        assert!(result.unwrap_err().errors[0].message.contains("unique"));
    }

    #[test]
    fn test_validate_unique_skips_field_without_parent_column() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)").unwrap();
        let fields = vec![FieldDefinition {
            name: "items".to_string(),
            field_type: FieldType::Array,
            unique: true,
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!(["a", "b"]));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok(), "Array field with unique=true should not run unique check");
    }
}
