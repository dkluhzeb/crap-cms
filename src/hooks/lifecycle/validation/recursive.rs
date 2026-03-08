use std::collections::HashMap;

use mlua::Lua;

use crate::core::field::{FieldDefinition, FieldType};
use crate::core::validate::FieldError;

use super::checks;
use super::sub_fields::validate_sub_fields_inner;

/// Recursive validation with prefix support for arbitrary nesting.
/// Group accumulates prefix (`group__`), Row/Collapsible/Tabs pass through.
pub(super) fn validate_fields_recursive(
    lua: &Lua,
    fields: &[FieldDefinition],
    data: &HashMap<String, serde_json::Value>,
    conn: &rusqlite::Connection,
    table: &str,
    exclude_id: Option<&str>,
    is_draft: bool,
    prefix: &str,
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
                    lua, &field.fields, data, conn, table, exclude_id, is_draft, &new_prefix, errors,
                );
            }
            FieldType::Row | FieldType::Collapsible => {
                validate_fields_recursive(
                    lua, &field.fields, data, conn, table, exclude_id, is_draft, prefix, errors,
                );
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    validate_fields_recursive(
                        lua, &tab.fields, data, conn, table, exclude_id, is_draft, prefix, errors,
                    );
                }
            }
            FieldType::Join => {
                // Virtual field — no data to validate
            }
            _ => {
                validate_scalar_field(lua, field, data, conn, table, exclude_id, is_draft, prefix, errors);
            }
        }
    }
}

/// Validate a single scalar field (not Group/Row/Collapsible/Tabs).
/// Dispatches to individual check functions in `checks` module.
fn validate_scalar_field(
    lua: &Lua,
    field: &FieldDefinition,
    data: &HashMap<String, serde_json::Value>,
    conn: &rusqlite::Connection,
    table: &str,
    exclude_id: Option<&str>,
    is_draft: bool,
    prefix: &str,
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

    checks::check_required(field, &data_key, value, is_empty, is_draft, is_update, errors);
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
                    let block_type = row_obj.get("_block_type")
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

    checks::check_unique(field, &data_key, value, is_empty, conn, table, exclude_id, errors);
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
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, seo__title TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "seo".to_string(),
            field_type: FieldType::Group,
            fields: vec![FieldDefinition {
                name: "title".to_string(),
                required: true,
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("seo__title".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.errors[0].field, "seo__title");
    }

    #[test]
    fn test_validate_required_inside_collapsible() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, notes TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "extra".to_string(),
            field_type: FieldType::Collapsible,
            fields: vec![FieldDefinition {
                name: "notes".to_string(),
                required: true,
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("notes".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().errors[0].field, "notes");
    }

    #[test]
    fn test_validate_required_inside_tabs() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, body TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "layout".to_string(),
            field_type: FieldType::Tabs,
            tabs: vec![FieldTab::new("Content", vec![FieldDefinition {
                    name: "body".to_string(),
                    required: true,
                    ..Default::default()
                }])],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("body".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().errors[0].field, "body");
    }

    #[test]
    fn test_validate_group_inside_tabs_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, seo__title TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "layout".to_string(),
            field_type: FieldType::Tabs,
            tabs: vec![FieldTab::new("SEO", vec![FieldDefinition {
                    name: "seo".to_string(),
                    field_type: FieldType::Group,
                    fields: vec![FieldDefinition {
                        name: "title".to_string(),
                        required: true,
                        ..Default::default()
                    }],
                    ..Default::default()
                }])],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("seo__title".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().errors[0].field, "seo__title");
    }

    #[test]
    fn test_validate_group_inside_collapsible_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, seo__title TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "extra".to_string(),
            field_type: FieldType::Collapsible,
            fields: vec![FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                fields: vec![FieldDefinition {
                    name: "title".to_string(),
                    required: true,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("seo__title".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().errors[0].field, "seo__title");
    }

    #[test]
    fn test_validate_date_inside_tabs() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, publish_date TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "layout".to_string(),
            field_type: FieldType::Tabs,
            tabs: vec![FieldTab::new("Meta", vec![FieldDefinition {
                    name: "publish_date".to_string(),
                    field_type: FieldType::Date,
                    ..Default::default()
                }])],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("publish_date".to_string(), json!("not-a-date"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("valid date"));
    }

    #[test]
    fn test_validate_unique_inside_tabs() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE test (id TEXT PRIMARY KEY, slug TEXT);
             INSERT INTO test (id, slug) VALUES ('existing', 'taken');"
        ).unwrap();
        let fields = vec![FieldDefinition {
            name: "layout".to_string(),
            field_type: FieldType::Tabs,
            tabs: vec![FieldTab::new("Meta", vec![FieldDefinition {
                    name: "slug".to_string(),
                    unique: true,
                    ..Default::default()
                }])],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("slug".to_string(), json!("taken"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("unique"));
    }

    #[test]
    fn test_validate_custom_function_inside_tabs() {
        let lua = mlua::Lua::new();
        lua.load(r#"
            package.loaded["validators"] = package.loaded["validators"] or {}
            package.loaded["validators"].validate_tabs_field = function(value, ctx)
                if value == "bad" then return "tabs validation error" end
                return true
            end
        "#).exec().unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, body TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "layout".to_string(),
            field_type: FieldType::Tabs,
            tabs: vec![FieldTab::new("Content", vec![FieldDefinition {
                    name: "body".to_string(),
                    validate: Some("validators.validate_tabs_field".to_string()),
                    ..Default::default()
                }])],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("body".to_string(), json!("bad"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("tabs validation error"));
    }

    #[test]
    fn test_validate_deeply_nested_tabs_collapsible_group() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, og__title TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "layout".to_string(),
            field_type: FieldType::Tabs,
            tabs: vec![FieldTab::new("Advanced", vec![FieldDefinition {
                    name: "advanced".to_string(),
                    field_type: FieldType::Collapsible,
                    fields: vec![FieldDefinition {
                        name: "og".to_string(),
                        field_type: FieldType::Group,
                        fields: vec![FieldDefinition {
                            name: "title".to_string(),
                            required: true,
                            ..Default::default()
                        }],
                        ..Default::default()
                    }],
                    ..Default::default()
                }])],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("og__title".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err(), "Deeply nested Group inside Collapsible inside Tabs should validate");
        assert_eq!(result.unwrap_err().errors[0].field, "og__title");
    }

    #[test]
    fn test_validate_layout_fields_skipped_for_drafts() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, body TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "layout".to_string(),
            field_type: FieldType::Tabs,
            tabs: vec![FieldTab::new("Content", vec![FieldDefinition {
                    name: "body".to_string(),
                    required: true,
                    ..Default::default()
                }])],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("body".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, true);
        assert!(result.is_ok(), "Draft saves should skip required checks in layout fields");
    }

    // ── Group containing layout fields ─────

    #[test]
    fn test_validate_group_containing_row_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, meta__title TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "meta".to_string(),
            field_type: FieldType::Group,
            fields: vec![FieldDefinition {
                name: "r".to_string(),
                field_type: FieldType::Row,
                fields: vec![FieldDefinition {
                    name: "title".to_string(),
                    required: true,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("meta__title".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err(), "Group→Row: required field should fail");
        assert_eq!(result.unwrap_err().errors[0].field, "meta__title");
    }

    #[test]
    fn test_validate_group_containing_collapsible_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, seo__robots TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "seo".to_string(),
            field_type: FieldType::Group,
            fields: vec![FieldDefinition {
                name: "c".to_string(),
                field_type: FieldType::Collapsible,
                fields: vec![FieldDefinition {
                    name: "robots".to_string(),
                    required: true,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("seo__robots".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err(), "Group→Collapsible: required field should fail");
        assert_eq!(result.unwrap_err().errors[0].field, "seo__robots");
    }

    #[test]
    fn test_validate_group_containing_tabs_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, settings__theme TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "settings".to_string(),
            field_type: FieldType::Group,
            fields: vec![FieldDefinition {
                name: "t".to_string(),
                field_type: FieldType::Tabs,
                tabs: vec![FieldTab::new("General", vec![FieldDefinition {
                        name: "theme".to_string(),
                        required: true,
                        ..Default::default()
                    }])],
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("settings__theme".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err(), "Group→Tabs: required field should fail");
        assert_eq!(result.unwrap_err().errors[0].field, "settings__theme");
    }

    #[test]
    fn test_validate_group_tabs_group_three_levels_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, outer__inner__deep TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "outer".to_string(),
            field_type: FieldType::Group,
            fields: vec![FieldDefinition {
                name: "t".to_string(),
                field_type: FieldType::Tabs,
                tabs: vec![FieldTab::new("Tab", vec![FieldDefinition {
                        name: "inner".to_string(),
                        field_type: FieldType::Group,
                        fields: vec![FieldDefinition {
                            name: "deep".to_string(),
                            required: true,
                            ..Default::default()
                        }],
                        ..Default::default()
                    }])],
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("outer__inner__deep".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err(), "Group→Tabs→Group: required field should fail");
        assert_eq!(result.unwrap_err().errors[0].field, "outer__inner__deep");
    }

    #[test]
    fn test_validate_group_containing_tabs_unique() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE test (id TEXT PRIMARY KEY, config__slug TEXT);
             INSERT INTO test (id, config__slug) VALUES ('existing', 'taken');"
        ).unwrap();
        let fields = vec![FieldDefinition {
            name: "config".to_string(),
            field_type: FieldType::Group,
            fields: vec![FieldDefinition {
                name: "t".to_string(),
                field_type: FieldType::Tabs,
                tabs: vec![FieldTab::new("Tab", vec![FieldDefinition {
                        name: "slug".to_string(),
                        unique: true,
                        ..Default::default()
                    }])],
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("config__slug".to_string(), json!("taken"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err(), "Group→Tabs: unique field should fail on duplicate");
        assert_eq!(result.unwrap_err().errors[0].field, "config__slug");
    }

    #[test]
    fn test_validate_group_containing_row_date_format() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, meta__date TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "meta".to_string(),
            field_type: FieldType::Group,
            fields: vec![FieldDefinition {
                name: "r".to_string(),
                field_type: FieldType::Row,
                fields: vec![FieldDefinition {
                    name: "date".to_string(),
                    field_type: FieldType::Date,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("meta__date".to_string(), json!("not-a-date"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err(), "Group→Row: invalid date should fail");
        assert_eq!(result.unwrap_err().errors[0].field, "meta__date");
    }

    #[test]
    fn test_validate_group_containing_row_valid_passes() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, meta__title TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "meta".to_string(),
            field_type: FieldType::Group,
            fields: vec![FieldDefinition {
                name: "r".to_string(),
                field_type: FieldType::Row,
                fields: vec![FieldDefinition {
                    name: "title".to_string(),
                    required: true,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("meta__title".to_string(), json!("Valid Title"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok(), "Group→Row: valid data should pass");
    }

    #[test]
    fn join_field_skipped_in_validation() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)").unwrap();
        let fields = vec![FieldDefinition {
            name: "posts".to_string(),
            field_type: FieldType::Join,
            required: true,
            join: Some(JoinConfig::new("posts", "author")),
            ..Default::default()
        }];
        let data = HashMap::new();
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok(), "Join field should be skipped entirely during validation");
    }

    #[test]
    fn test_validate_nested_group_in_group_prefix() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, outer__inner__field TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "outer".to_string(),
            field_type: FieldType::Group,
            fields: vec![FieldDefinition {
                name: "inner".to_string(),
                field_type: FieldType::Group,
                fields: vec![FieldDefinition {
                    name: "field".to_string(),
                    required: true,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("outer__inner__field".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err(), "Nested group prefix should be outer__inner__field");
        assert_eq!(result.unwrap_err().errors[0].field, "outer__inner__field");
    }

    #[test]
    fn test_validate_date_inside_collapsible_top_level() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, pub_date TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "extra".to_string(),
            field_type: FieldType::Collapsible,
            fields: vec![FieldDefinition {
                name: "pub_date".to_string(),
                field_type: FieldType::Date,
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("pub_date".to_string(), json!("not-a-date"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err(), "Invalid date inside collapsible at top-level should fail");
        assert!(result.unwrap_err().errors[0].message.contains("valid date"));
    }

    #[test]
    fn test_validate_date_inside_row_top_level() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, event_date TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "layout".to_string(),
            field_type: FieldType::Row,
            fields: vec![FieldDefinition {
                name: "event_date".to_string(),
                field_type: FieldType::Date,
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("event_date".to_string(), json!("not-a-date"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err(), "Invalid date inside row at top-level should fail");
        assert!(result.unwrap_err().errors[0].message.contains("valid date"));
    }
}
