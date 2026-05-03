use serde_json::Value;

use crate::core::{FieldDefinition, FieldType, validate::FieldError};
use std::collections::HashMap;

/// Validate email format (only if non-empty).
pub(crate) fn check_email_format(
    field: &FieldDefinition,
    data_key: &str,
    value: Option<&Value>,
    is_empty: bool,
    errors: &mut Vec<FieldError>,
) {
    if field.field_type != FieldType::Email || is_empty {
        return;
    }

    if let Some(Value::String(s)) = value
        && !is_valid_email_format(s)
    {
        errors.push(FieldError::with_key(
            data_key.to_owned(),
            format!("{} is not a valid email address", field.name),
            "validation.email",
            HashMap::from([("field".to_string(), field.name.clone())]),
        ));
    }
}

/// Check if a string looks like a valid email address.
///
/// Validates: exactly one @, non-empty local/domain, domain has a dot,
/// no whitespace, no leading/trailing/consecutive dots in local part.
pub fn is_valid_email_format(value: &str) -> bool {
    let parts: Vec<&str> = value.splitn(2, '@').collect();

    if parts.len() != 2 {
        return false;
    }
    let local = parts[0];
    let domain = parts[1];

    !local.is_empty()
        && !domain.is_empty()
        && domain.contains('.')
        && !local.contains(char::is_whitespace)
        && !domain.contains(char::is_whitespace)
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..")
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains("..")
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::hooks::lifecycle::validation::{ValidationCtx, validate_fields_inner};
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn test_validate_email_format_valid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, email TEXT)")
            .unwrap();
        let fields = vec![FieldDefinition::builder("email", FieldType::Email).build()];
        let mut data = HashMap::new();
        data.insert("email".to_string(), json!("user@example.com"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_email_format_invalid_no_at() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, email TEXT)")
            .unwrap();
        let fields = vec![FieldDefinition::builder("email", FieldType::Email).build()];
        let mut data = HashMap::new();
        data.insert("email".to_string(), json!("not-an-email"));
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
                .contains("valid email")
        );
    }

    #[test]
    fn test_validate_email_format_invalid_no_domain() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, email TEXT)")
            .unwrap();
        let fields = vec![FieldDefinition::builder("email", FieldType::Email).build()];
        let mut data = HashMap::new();
        data.insert("email".to_string(), json!("user@"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_email_format_skipped_for_empty() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, email TEXT)")
            .unwrap();
        let fields = vec![FieldDefinition::builder("email", FieldType::Email).build()];
        let mut data = HashMap::new();
        data.insert("email".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok(), "Email validation should skip empty values");
    }

    #[test]
    fn test_email_format_valid_addresses() {
        assert!(is_valid_email_format("user@example.com"));
        assert!(is_valid_email_format("a@b.c"));
        assert!(is_valid_email_format("test+tag@domain.org"));
        assert!(is_valid_email_format("user.name@sub.domain.com"));
    }

    #[test]
    fn test_email_format_invalid_addresses() {
        assert!(!is_valid_email_format(""));
        assert!(!is_valid_email_format("no-at-sign"));
        assert!(!is_valid_email_format("@no-local.com"));
        assert!(!is_valid_email_format("user@"));
        assert!(!is_valid_email_format("user@nodot"));
        assert!(!is_valid_email_format("user @space.com"));
        assert!(!is_valid_email_format("user@ space.com"));
    }

    #[test]
    fn test_email_format_whitespace_in_local_part() {
        assert!(!is_valid_email_format("user name@domain.com"));
    }

    #[test]
    fn test_email_format_whitespace_in_domain() {
        assert!(!is_valid_email_format("user@do main.com"));
    }

    #[test]
    fn test_email_format_rejects_leading_dot_in_local() {
        assert!(!is_valid_email_format(".user@example.com"));
    }

    #[test]
    fn test_email_format_rejects_trailing_dot_in_local() {
        assert!(!is_valid_email_format("user.@example.com"));
    }

    #[test]
    fn test_email_format_rejects_consecutive_dots_in_local() {
        assert!(!is_valid_email_format("user..name@example.com"));
    }

    #[test]
    fn test_email_format_rejects_invalid_domain_dots() {
        assert!(!is_valid_email_format("user@.example.com"));
        assert!(!is_valid_email_format("user@example.com."));
        assert!(!is_valid_email_format("user@example..com"));
    }
}
