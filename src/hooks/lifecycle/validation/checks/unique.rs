use serde_json::Value;

use std::collections::HashMap;

use crate::{
    core::{FieldDefinition, validate::FieldError},
    db::query,
    hooks::ValidationCtx,
};

/// Check unique constraint (only if value is non-empty and field has a parent column).
/// `col_name` is the actual DB column to query (may differ from `data_key` for localized fields).
pub(crate) fn check_unique(
    field: &FieldDefinition,
    data_key: &str,
    col_name: &str,
    value: Option<&Value>,
    is_empty: bool,
    ctx: &ValidationCtx,
    errors: &mut Vec<FieldError>,
) {
    if !field.unique || is_empty || !field.has_parent_column() {
        return;
    }
    let value_str = match value {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    };
    match query::count_where_field_eq(
        ctx.conn,
        ctx.table,
        col_name,
        &value_str,
        ctx.exclude_id,
        ctx.soft_delete,
    ) {
        Ok(count) if count > 0 => {
            errors.push(FieldError::with_key(
                data_key.to_owned(),
                format!("{} must be unique", field.name),
                "validation.unique",
                HashMap::from([("field".to_string(), field.name.clone())]),
            ));
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!("Unique check failed for {}.{}: {}", ctx.table, data_key, e);
            errors.push(FieldError::with_key(
                data_key.to_owned(),
                format!(
                    "Could not verify uniqueness for {} (database error)",
                    field.name
                ),
                "validation.unique_check_failed",
                HashMap::from([("field".to_string(), field.name.clone())]),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::config::LocaleConfig;
    use crate::core::field::{FieldDefinition, FieldType};
    use crate::db::query::LocaleContext;
    use crate::hooks::lifecycle::validation::{ValidationCtx, validate_fields_inner};
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn test_validate_unique_check() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE test (id TEXT PRIMARY KEY, email TEXT);
             INSERT INTO test (id, email) VALUES ('existing', 'taken@test.com');",
        )
        .unwrap();
        let fields = vec![
            FieldDefinition::builder("email", FieldType::Text)
                .unique(true)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("email".to_string(), json!("taken@test.com"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("unique"));
    }

    #[test]
    fn test_validate_unique_check_excludes_self() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE test (id TEXT PRIMARY KEY, email TEXT);
             INSERT INTO test (id, email) VALUES ('self', 'me@test.com');",
        )
        .unwrap();
        let fields = vec![
            FieldDefinition::builder("email", FieldType::Text)
                .unique(true)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("email".to_string(), json!("me@test.com"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test")
                .exclude_id(Some("self"))
                .build(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_unique_check_skips_empty_value() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE test (id TEXT PRIMARY KEY, slug TEXT);
             INSERT INTO test (id, slug) VALUES ('a', '');",
        )
        .unwrap();
        let fields = vec![
            FieldDefinition::builder("slug", FieldType::Text)
                .unique(true)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("slug".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_ok(),
            "Unique check should not fire on empty value"
        );
    }

    #[test]
    fn test_validate_unique_check_with_number_value() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE test (id TEXT PRIMARY KEY, rank REAL);
             INSERT INTO test (id, rank) VALUES ('existing', 42);",
        )
        .unwrap();
        let fields = vec![
            FieldDefinition::builder("rank", FieldType::Number)
                .unique(true)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("rank".to_string(), json!(42));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "Duplicate number value should fail unique check"
        );
        assert!(result.unwrap_err().errors[0].message.contains("unique"));
    }

    #[test]
    fn test_validate_unique_skips_field_without_parent_column() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .unique(true)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!(["a", "b"]));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_ok(),
            "Array field with unique=true should not run unique check"
        );
    }

    #[test]
    fn test_validate_unique_localized_field_checks_suffixed_column() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        // Create table with locale-suffixed column (as migration would)
        conn.execute_batch(
            "CREATE TABLE test (id TEXT PRIMARY KEY, slug__en TEXT, slug__de TEXT);
             INSERT INTO test (id, slug__en, slug__de) VALUES ('existing', 'taken-en', 'taken-de');"
        ).unwrap();
        let fields = vec![
            FieldDefinition::builder("slug", FieldType::Text)
                .unique(true)
                .localized(true)
                .build(),
        ];
        let locale_cfg = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };
        let locale_ctx = LocaleContext::from_locale_string(Some("en"), &locale_cfg).unwrap();

        // Duplicate value in the en column should fail
        let mut data = HashMap::new();
        data.insert("slug".to_string(), json!("taken-en"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test")
                .locale_ctx(Some(&locale_ctx))
                .build(),
        );
        assert!(
            result.is_err(),
            "Localized unique field should detect duplicate in suffixed column"
        );
        assert!(result.unwrap_err().errors[0].message.contains("unique"));

        // Non-duplicate value should pass
        let mut data = HashMap::new();
        data.insert("slug".to_string(), json!("fresh"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test")
                .locale_ctx(Some(&locale_ctx))
                .build(),
        );
        assert!(
            result.is_ok(),
            "Localized unique field with fresh value should pass"
        );
    }

    #[test]
    fn test_validate_unique_inherited_localized_from_group() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        // Group is localized, child field inherits localization
        conn.execute_batch(
            "CREATE TABLE test (id TEXT PRIMARY KEY, seo__slug__en TEXT, seo__slug__de TEXT);
             INSERT INTO test (id, seo__slug__en) VALUES ('existing', 'taken');",
        )
        .unwrap();
        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .localized(true)
                .fields(vec![
                    FieldDefinition::builder("slug", FieldType::Text)
                        .unique(true)
                        .build(),
                ])
                .build(),
        ];
        let locale_cfg = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };
        let locale_ctx = LocaleContext::from_locale_string(Some("en"), &locale_cfg).unwrap();

        let mut data = HashMap::new();
        data.insert("seo__slug".to_string(), json!("taken"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test")
                .locale_ctx(Some(&locale_ctx))
                .build(),
        );
        assert!(
            result.is_err(),
            "Inherited localized unique field should detect duplicate"
        );
        assert!(result.unwrap_err().errors[0].message.contains("unique"));
        // Error field should use the data key, not the DB column
        let mut data = HashMap::new();
        data.insert("seo__slug".to_string(), json!("taken"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test")
                .locale_ctx(Some(&locale_ctx))
                .build(),
        );
        assert_eq!(result.unwrap_err().errors[0].field, "seo__slug");
    }

    #[test]
    fn test_validate_unique_soft_delete_excludes_deleted_rows() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE test (id TEXT PRIMARY KEY, slug TEXT, _deleted_at TEXT);
             INSERT INTO test (id, slug, _deleted_at) VALUES ('deleted1', 'taken', '2024-01-01');",
        )
        .unwrap();
        let fields = vec![
            FieldDefinition::builder("slug", FieldType::Text)
                .unique(true)
                .build(),
        ];

        // With soft_delete=true, the soft-deleted row should be excluded from the unique check
        let mut data = HashMap::new();
        data.insert("slug".to_string(), json!("taken"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test")
                .soft_delete(true)
                .build(),
        );
        assert!(
            result.is_ok(),
            "Unique check with soft_delete=true should ignore soft-deleted rows"
        );

        // With soft_delete=false (the old default), the same value would fail
        let mut data = HashMap::new();
        data.insert("slug".to_string(), json!("taken"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test")
                .soft_delete(false)
                .build(),
        );
        assert!(
            result.is_err(),
            "Unique check with soft_delete=false should include soft-deleted rows"
        );
    }

    #[test]
    fn test_validate_unique_localized_without_locale_ctx_uses_bare_column() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        // No locale context = bare column name (backward compat)
        conn.execute_batch(
            "CREATE TABLE test (id TEXT PRIMARY KEY, slug TEXT);
             INSERT INTO test (id, slug) VALUES ('existing', 'taken');",
        )
        .unwrap();
        let fields = vec![
            FieldDefinition::builder("slug", FieldType::Text)
                .unique(true)
                .localized(true)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("slug".to_string(), json!("taken"));
        // No locale_ctx → falls back to bare column
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "Without locale context, should check bare column"
        );
    }

    /// Regression: when the DB query for uniqueness fails (e.g. missing table),
    /// the unique check must produce a validation error — not silently pass.
    #[test]
    fn test_validate_unique_db_error_produces_validation_error() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        // Table "test" does NOT exist — query will fail
        let fields = vec![
            FieldDefinition::builder("email", FieldType::Text)
                .unique(true)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("email".to_string(), json!("any@test.com"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "DB error during unique check must fail validation"
        );
        let errs = result.unwrap_err().errors;
        assert_eq!(errs.len(), 1);
        assert_eq!(
            errs[0].key.as_deref(),
            Some("validation.unique_check_failed")
        );
    }
}
