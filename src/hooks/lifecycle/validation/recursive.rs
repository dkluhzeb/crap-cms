use std::collections::HashMap;

use mlua::Lua;

use crate::core::field::{FieldDefinition, FieldType};
use crate::core::validate::FieldError;
use crate::db::query::sanitize_locale;
use crate::db::query::{LocaleContext, LocaleMode};

use super::checks;
use super::sub_fields::validate_sub_fields_inner;

/// Recursive validation with prefix support for arbitrary nesting.
/// Group accumulates prefix (`group__`), Row/Collapsible/Tabs pass through.
/// `locale_ctx` and `inherited_localized` track locale state for unique checks.
#[allow(clippy::too_many_arguments)]
pub(super) fn validate_fields_recursive(
    lua: &Lua,
    fields: &[FieldDefinition],
    data: &HashMap<String, serde_json::Value>,
    conn: &rusqlite::Connection,
    table: &str,
    exclude_id: Option<&str>,
    is_draft: bool,
    prefix: &str,
    locale_ctx: Option<&LocaleContext>,
    inherited_localized: bool,
    errors: &mut Vec<FieldError>,
) {
    for field in fields {
        match field.field_type {
            FieldType::Group => {
                let new_prefix = if prefix.is_empty() {
                    field.name.clone()
                } else {
                    format!("{}__{}", prefix, field.name)
                };
                validate_fields_recursive(
                    lua,
                    &field.fields,
                    data,
                    conn,
                    table,
                    exclude_id,
                    is_draft,
                    &new_prefix,
                    locale_ctx,
                    inherited_localized || field.localized,
                    errors,
                );
            }
            FieldType::Row | FieldType::Collapsible => {
                validate_fields_recursive(
                    lua,
                    &field.fields,
                    data,
                    conn,
                    table,
                    exclude_id,
                    is_draft,
                    prefix,
                    locale_ctx,
                    inherited_localized,
                    errors,
                );
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    validate_fields_recursive(
                        lua,
                        &tab.fields,
                        data,
                        conn,
                        table,
                        exclude_id,
                        is_draft,
                        prefix,
                        locale_ctx,
                        inherited_localized,
                        errors,
                    );
                }
            }
            FieldType::Join => {
                // Virtual field — no data to validate
            }
            _ => {
                validate_scalar_field(
                    lua,
                    field,
                    data,
                    conn,
                    table,
                    exclude_id,
                    is_draft,
                    prefix,
                    locale_ctx,
                    inherited_localized,
                    errors,
                );
            }
        }
    }
}

/// Validate a single scalar field (not Group/Row/Collapsible/Tabs).
/// Dispatches to individual check functions in `checks` module.
#[allow(clippy::too_many_arguments)]
fn validate_scalar_field(
    lua: &Lua,
    field: &FieldDefinition,
    data: &HashMap<String, serde_json::Value>,
    conn: &rusqlite::Connection,
    table: &str,
    exclude_id: Option<&str>,
    is_draft: bool,
    prefix: &str,
    locale_ctx: Option<&LocaleContext>,
    inherited_localized: bool,
    errors: &mut Vec<FieldError>,
) {
    let data_key = if prefix.is_empty() {
        field.name.clone()
    } else {
        format!("{}__{}", prefix, field.name)
    };

    let value = data.get(&data_key);
    let is_empty = match value {
        None => true,
        Some(serde_json::Value::Null) => true,
        Some(serde_json::Value::String(s)) => s.is_empty(),
        _ => false,
    };
    let is_update = exclude_id.is_some();

    checks::check_required(
        field, &data_key, value, is_empty, is_draft, is_update, errors,
    );
    checks::check_row_bounds(field, &data_key, value, is_draft, errors);

    // Validate sub-fields within Array/Blocks rows
    if !is_draft && matches!(field.field_type, FieldType::Array | FieldType::Blocks) {
        if let Some(serde_json::Value::Array(rows)) = value {
            for (idx, row) in rows.iter().enumerate() {
                let row_obj = match row.as_object() {
                    Some(obj) => obj,
                    None => continue,
                };
                let sub_fields: &[FieldDefinition] = if field.field_type == FieldType::Blocks {
                    let block_type = row_obj
                        .get("_block_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    match field.blocks.iter().find(|b| b.block_type == block_type) {
                        Some(bd) => &bd.fields,
                        None => continue,
                    }
                } else {
                    &field.fields
                };
                validate_sub_fields_inner(lua, sub_fields, row_obj, &data_key, idx, table, errors);
            }
        }
    }

    // Compute the actual DB column name for the unique check.
    // Localized fields store data in suffixed columns (e.g., slug__en).
    let is_localized = (inherited_localized || field.localized) && locale_ctx.is_some();
    let col_name = if is_localized {
        let ctx = locale_ctx.unwrap();
        let locale = match &ctx.mode {
            LocaleMode::Single(l) => l.as_str(),
            _ => ctx.config.default_locale.as_str(),
        };
        format!("{}__{}", data_key, sanitize_locale(locale))
    } else {
        data_key.clone()
    };

    checks::check_unique(
        field, &data_key, &col_name, value, is_empty, conn, table, exclude_id, errors,
    );
    checks::check_length_bounds(field, &data_key, value, is_empty, errors);
    checks::check_numeric_bounds(field, &data_key, value, is_empty, errors);
    checks::check_email_format(field, &data_key, value, is_empty, errors);
    checks::check_option_valid(field, &data_key, value, is_empty, errors);
    checks::check_has_many_elements(field, &data_key, value, is_empty, errors);
    checks::check_date_field(field, &data_key, value, is_empty, errors);
    checks::check_custom_validate(lua, field, &data_key, value, data, table, errors);
}

#[cfg(test)]
mod tests {
    use crate::core::field::{FieldDefinition, FieldTab, FieldType, JoinConfig};
    use crate::hooks::lifecycle::validation::validate_fields_inner;
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn test_validate_group_subfield_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, seo__title TEXT)")
            .unwrap();
        let fields = vec![FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![FieldDefinition::builder("title", FieldType::Text)
                .required(true)
                .build()])
            .build()];
        let mut data = HashMap::new();
        data.insert("seo__title".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false, None);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.errors[0].field, "seo__title");
    }

    #[test]
    fn test_validate_required_inside_collapsible() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, notes TEXT)")
            .unwrap();
        let fields = vec![FieldDefinition::builder("extra", FieldType::Collapsible)
            .fields(vec![FieldDefinition::builder("notes", FieldType::Text)
                .required(true)
                .build()])
            .build()];
        let mut data = HashMap::new();
        data.insert("notes".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false, None);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().errors[0].field, "notes");
    }

    #[test]
    fn test_validate_required_inside_tabs() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, body TEXT)")
            .unwrap();
        let fields = vec![FieldDefinition::builder("layout", FieldType::Tabs)
            .tabs(vec![FieldTab::new(
                "Content",
                vec![FieldDefinition::builder("body", FieldType::Text)
                    .required(true)
                    .build()],
            )])
            .build()];
        let mut data = HashMap::new();
        data.insert("body".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false, None);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().errors[0].field, "body");
    }

    #[test]
    fn test_validate_group_inside_tabs_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, seo__title TEXT)")
            .unwrap();
        let fields = vec![FieldDefinition::builder("layout", FieldType::Tabs)
            .tabs(vec![FieldTab::new(
                "SEO",
                vec![FieldDefinition::builder("seo", FieldType::Group)
                    .fields(vec![FieldDefinition::builder("title", FieldType::Text)
                        .required(true)
                        .build()])
                    .build()],
            )])
            .build()];
        let mut data = HashMap::new();
        data.insert("seo__title".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false, None);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().errors[0].field, "seo__title");
    }

    #[test]
    fn test_validate_group_inside_collapsible_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, seo__title TEXT)")
            .unwrap();
        let fields = vec![FieldDefinition::builder("extra", FieldType::Collapsible)
            .fields(vec![FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![FieldDefinition::builder("title", FieldType::Text)
                    .required(true)
                    .build()])
                .build()])
            .build()];
        let mut data = HashMap::new();
        data.insert("seo__title".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false, None);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().errors[0].field, "seo__title");
    }

    #[test]
    fn test_validate_date_inside_tabs() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, publish_date TEXT)")
            .unwrap();
        let fields = vec![FieldDefinition::builder("layout", FieldType::Tabs)
            .tabs(vec![FieldTab::new(
                "Meta",
                vec![FieldDefinition::builder("publish_date", FieldType::Date).build()],
            )])
            .build()];
        let mut data = HashMap::new();
        data.insert("publish_date".to_string(), json!("not-a-date"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("valid date"));
    }

    #[test]
    fn test_validate_unique_inside_tabs() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE test (id TEXT PRIMARY KEY, slug TEXT);
             INSERT INTO test (id, slug) VALUES ('existing', 'taken');",
        )
        .unwrap();
        let fields = vec![FieldDefinition::builder("layout", FieldType::Tabs)
            .tabs(vec![FieldTab::new(
                "Meta",
                vec![FieldDefinition::builder("slug", FieldType::Text)
                    .unique(true)
                    .build()],
            )])
            .build()];
        let mut data = HashMap::new();
        data.insert("slug".to_string(), json!("taken"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("unique"));
    }

    #[test]
    fn test_validate_custom_function_inside_tabs() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"
            package.loaded["validators"] = package.loaded["validators"] or {}
            package.loaded["validators"].validate_tabs_field = function(value, ctx)
                if value == "bad" then return "tabs validation error" end
                return true
            end
        "#,
        )
        .exec()
        .unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, body TEXT)")
            .unwrap();
        let fields = vec![FieldDefinition::builder("layout", FieldType::Tabs)
            .tabs(vec![FieldTab::new(
                "Content",
                vec![FieldDefinition::builder("body", FieldType::Text)
                    .validate("validators.validate_tabs_field")
                    .build()],
            )])
            .build()];
        let mut data = HashMap::new();
        data.insert("body".to_string(), json!("bad"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0]
            .message
            .contains("tabs validation error"));
    }

    #[test]
    fn test_validate_deeply_nested_tabs_collapsible_group() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, og__title TEXT)")
            .unwrap();
        let fields = vec![FieldDefinition::builder("layout", FieldType::Tabs)
            .tabs(vec![FieldTab::new(
                "Advanced",
                vec![FieldDefinition::builder("advanced", FieldType::Collapsible)
                    .fields(vec![FieldDefinition::builder("og", FieldType::Group)
                        .fields(vec![FieldDefinition::builder("title", FieldType::Text)
                            .required(true)
                            .build()])
                        .build()])
                    .build()],
            )])
            .build()];
        let mut data = HashMap::new();
        data.insert("og__title".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false, None);
        assert!(
            result.is_err(),
            "Deeply nested Group inside Collapsible inside Tabs should validate"
        );
        assert_eq!(result.unwrap_err().errors[0].field, "og__title");
    }

    #[test]
    fn test_validate_layout_fields_skipped_for_drafts() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, body TEXT)")
            .unwrap();
        let fields = vec![FieldDefinition::builder("layout", FieldType::Tabs)
            .tabs(vec![FieldTab::new(
                "Content",
                vec![FieldDefinition::builder("body", FieldType::Text)
                    .required(true)
                    .build()],
            )])
            .build()];
        let mut data = HashMap::new();
        data.insert("body".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, true, None);
        assert!(
            result.is_ok(),
            "Draft saves should skip required checks in layout fields"
        );
    }

    // ── Group containing layout fields ─────

    #[test]
    fn test_validate_group_containing_row_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, meta__title TEXT)")
            .unwrap();
        let fields = vec![FieldDefinition::builder("meta", FieldType::Group)
            .fields(vec![FieldDefinition::builder("r", FieldType::Row)
                .fields(vec![FieldDefinition::builder("title", FieldType::Text)
                    .required(true)
                    .build()])
                .build()])
            .build()];
        let mut data = HashMap::new();
        data.insert("meta__title".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false, None);
        assert!(result.is_err(), "Group→Row: required field should fail");
        assert_eq!(result.unwrap_err().errors[0].field, "meta__title");
    }

    #[test]
    fn test_validate_group_containing_collapsible_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, seo__robots TEXT)")
            .unwrap();
        let fields = vec![FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![FieldDefinition::builder("c", FieldType::Collapsible)
                .fields(vec![FieldDefinition::builder("robots", FieldType::Text)
                    .required(true)
                    .build()])
                .build()])
            .build()];
        let mut data = HashMap::new();
        data.insert("seo__robots".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false, None);
        assert!(
            result.is_err(),
            "Group→Collapsible: required field should fail"
        );
        assert_eq!(result.unwrap_err().errors[0].field, "seo__robots");
    }

    #[test]
    fn test_validate_group_containing_tabs_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, settings__theme TEXT)")
            .unwrap();
        let fields = vec![FieldDefinition::builder("settings", FieldType::Group)
            .fields(vec![FieldDefinition::builder("t", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "General",
                    vec![FieldDefinition::builder("theme", FieldType::Text)
                        .required(true)
                        .build()],
                )])
                .build()])
            .build()];
        let mut data = HashMap::new();
        data.insert("settings__theme".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false, None);
        assert!(result.is_err(), "Group→Tabs: required field should fail");
        assert_eq!(result.unwrap_err().errors[0].field, "settings__theme");
    }

    #[test]
    fn test_validate_group_tabs_group_three_levels_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, outer__inner__deep TEXT)")
            .unwrap();
        let fields = vec![FieldDefinition::builder("outer", FieldType::Group)
            .fields(vec![FieldDefinition::builder("t", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "Tab",
                    vec![FieldDefinition::builder("inner", FieldType::Group)
                        .fields(vec![FieldDefinition::builder("deep", FieldType::Text)
                            .required(true)
                            .build()])
                        .build()],
                )])
                .build()])
            .build()];
        let mut data = HashMap::new();
        data.insert("outer__inner__deep".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false, None);
        assert!(
            result.is_err(),
            "Group→Tabs→Group: required field should fail"
        );
        assert_eq!(result.unwrap_err().errors[0].field, "outer__inner__deep");
    }

    #[test]
    fn test_validate_group_containing_tabs_unique() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE test (id TEXT PRIMARY KEY, config__slug TEXT);
             INSERT INTO test (id, config__slug) VALUES ('existing', 'taken');",
        )
        .unwrap();
        let fields = vec![FieldDefinition::builder("config", FieldType::Group)
            .fields(vec![FieldDefinition::builder("t", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "Tab",
                    vec![FieldDefinition::builder("slug", FieldType::Text)
                        .unique(true)
                        .build()],
                )])
                .build()])
            .build()];
        let mut data = HashMap::new();
        data.insert("config__slug".to_string(), json!("taken"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false, None);
        assert!(
            result.is_err(),
            "Group→Tabs: unique field should fail on duplicate"
        );
        assert_eq!(result.unwrap_err().errors[0].field, "config__slug");
    }

    #[test]
    fn test_validate_group_containing_row_date_format() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, meta__date TEXT)")
            .unwrap();
        let fields = vec![FieldDefinition::builder("meta", FieldType::Group)
            .fields(vec![FieldDefinition::builder("r", FieldType::Row)
                .fields(vec![
                    FieldDefinition::builder("date", FieldType::Date).build()
                ])
                .build()])
            .build()];
        let mut data = HashMap::new();
        data.insert("meta__date".to_string(), json!("not-a-date"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false, None);
        assert!(result.is_err(), "Group→Row: invalid date should fail");
        assert_eq!(result.unwrap_err().errors[0].field, "meta__date");
    }

    #[test]
    fn test_validate_group_containing_row_valid_passes() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, meta__title TEXT)")
            .unwrap();
        let fields = vec![FieldDefinition::builder("meta", FieldType::Group)
            .fields(vec![FieldDefinition::builder("r", FieldType::Row)
                .fields(vec![FieldDefinition::builder("title", FieldType::Text)
                    .required(true)
                    .build()])
                .build()])
            .build()];
        let mut data = HashMap::new();
        data.insert("meta__title".to_string(), json!("Valid Title"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false, None);
        assert!(result.is_ok(), "Group→Row: valid data should pass");
    }

    #[test]
    fn join_field_skipped_in_validation() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![FieldDefinition::builder("posts", FieldType::Join)
            .required(true)
            .join(JoinConfig::new("posts", "author"))
            .build()];
        let data = HashMap::new();
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false, None);
        assert!(
            result.is_ok(),
            "Join field should be skipped entirely during validation"
        );
    }

    #[test]
    fn test_validate_nested_group_in_group_prefix() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, outer__inner__field TEXT)")
            .unwrap();
        let fields = vec![FieldDefinition::builder("outer", FieldType::Group)
            .fields(vec![FieldDefinition::builder("inner", FieldType::Group)
                .fields(vec![FieldDefinition::builder("field", FieldType::Text)
                    .required(true)
                    .build()])
                .build()])
            .build()];
        let mut data = HashMap::new();
        data.insert("outer__inner__field".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false, None);
        assert!(
            result.is_err(),
            "Nested group prefix should be outer__inner__field"
        );
        assert_eq!(result.unwrap_err().errors[0].field, "outer__inner__field");
    }

    #[test]
    fn test_validate_date_inside_collapsible_top_level() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, pub_date TEXT)")
            .unwrap();
        let fields = vec![FieldDefinition::builder("extra", FieldType::Collapsible)
            .fields(vec![
                FieldDefinition::builder("pub_date", FieldType::Date).build()
            ])
            .build()];
        let mut data = HashMap::new();
        data.insert("pub_date".to_string(), json!("not-a-date"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false, None);
        assert!(
            result.is_err(),
            "Invalid date inside collapsible at top-level should fail"
        );
        assert!(result.unwrap_err().errors[0].message.contains("valid date"));
    }

    #[test]
    fn test_validate_date_inside_row_top_level() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, event_date TEXT)")
            .unwrap();
        let fields = vec![FieldDefinition::builder("layout", FieldType::Row)
            .fields(vec![FieldDefinition::builder(
                "event_date",
                FieldType::Date,
            )
            .build()])
            .build()];
        let mut data = HashMap::new();
        data.insert("event_date".to_string(), json!("not-a-date"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false, None);
        assert!(
            result.is_err(),
            "Invalid date inside row at top-level should fail"
        );
        assert!(result.unwrap_err().errors[0].message.contains("valid date"));
    }
}
