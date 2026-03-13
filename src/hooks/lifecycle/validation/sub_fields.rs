use std::collections::HashMap;

use mlua::Lua;
use serde_json::{Map as JsonMap, Value};

use crate::core::{
    field::{FieldDefinition, FieldType},
    validate::FieldError,
};

use super::{checks::is_valid_date_format, custom::run_validate_function_inner};

/// Validate sub-fields within a single array/blocks row (inner, no mutex).
pub(super) fn validate_sub_fields_inner(
    lua: &Lua,
    sub_fields: &[FieldDefinition],
    row_obj: &JsonMap<String, Value>,
    parent_name: &str,
    idx: usize,
    table: &str,
    errors: &mut Vec<FieldError>,
) {
    let row_data: HashMap<String, Value> = row_obj
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    for sf in sub_fields {
        let sf_value = row_obj.get(&sf.name);
        let sf_empty = match sf_value {
            None => true,
            Some(Value::Null) => true,
            Some(Value::String(s)) => s.is_empty(),
            _ => false,
        };
        let qualified_name = format!("{}[{}][{}]", parent_name, idx, sf.name);

        if sf.required && sf_empty && sf.field_type != FieldType::Checkbox {
            errors.push(FieldError::with_key(
                qualified_name.clone(),
                format!("{} is required", sf.name),
                "validation.required",
                HashMap::from([("field".to_string(), sf.name.clone())]),
            ));
        }

        if sf.field_type == FieldType::Date
            && !sf_empty
            && let Some(Value::String(s)) = sf_value
            && !is_valid_date_format(s)
        {
            errors.push(FieldError::with_key(
                qualified_name.clone(),
                format!("{} is not a valid date format", sf.name),
                "validation.invalid_date",
                HashMap::from([("field".to_string(), sf.name.clone())]),
            ));
        }

        if let Some(ref validate_ref) = sf.validate
            && let Some(val) = sf_value
        {
            match run_validate_function_inner(lua, validate_ref, val, &row_data, table, &sf.name) {
                Ok(Some(err_msg)) => {
                    errors.push(FieldError::new(qualified_name.clone(), err_msg));
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!("Validate function '{}' error: {}", validate_ref, e);
                }
            }
        }

        if matches!(sf.field_type, FieldType::Array | FieldType::Blocks)
            && let Some(Value::Array(nested_rows)) = sf_value
        {
            let nested_parent = format!("{}[{}][{}]", parent_name, idx, sf.name);
            for (nested_idx, nested_row) in nested_rows.iter().enumerate() {
                if let Some(nested_obj) = nested_row.as_object() {
                    let nested_sub_fields: &[FieldDefinition] =
                        if sf.field_type == FieldType::Blocks {
                            let bt = nested_obj
                                .get("_block_type")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            match sf.blocks.iter().find(|b| b.block_type == bt) {
                                Some(bd) => &bd.fields,
                                None => continue,
                            }
                        } else {
                            &sf.fields
                        };
                    validate_sub_fields_inner(
                        lua,
                        nested_sub_fields,
                        nested_obj,
                        &nested_parent,
                        nested_idx,
                        table,
                        errors,
                    );
                }
            }
        }

        if sf.field_type == FieldType::Group {
            for gsf in &sf.fields {
                let group_key = format!("{}__{}", sf.name, gsf.name);
                let g_qualified = format!("{}[{}][{}]", parent_name, idx, group_key);
                validate_leaf_sub_field(
                    lua,
                    gsf,
                    row_obj.get(&group_key),
                    &g_qualified,
                    &row_data,
                    table,
                    errors,
                );
            }
        }

        // Row sub-fields within arrays use plain sub-field names (no prefix)
        if sf.field_type == FieldType::Row {
            for rsf in &sf.fields {
                let r_qualified = format!("{}[{}][{}]", parent_name, idx, rsf.name);
                validate_leaf_sub_field(
                    lua,
                    rsf,
                    row_obj.get(&rsf.name),
                    &r_qualified,
                    &row_data,
                    table,
                    errors,
                );
            }
        }

        // Collapsible sub-fields within arrays (same as Row)
        if sf.field_type == FieldType::Collapsible {
            for csf in &sf.fields {
                let c_qualified = format!("{}[{}][{}]", parent_name, idx, csf.name);
                validate_leaf_sub_field(
                    lua,
                    csf,
                    row_obj.get(&csf.name),
                    &c_qualified,
                    &row_data,
                    table,
                    errors,
                );
            }
        }

        // Tabs sub-fields within arrays (iterate tab.fields)
        if sf.field_type == FieldType::Tabs {
            for tab in &sf.tabs {
                for tsf in &tab.fields {
                    let t_qualified = format!("{}[{}][{}]", parent_name, idx, tsf.name);
                    validate_leaf_sub_field(
                        lua,
                        tsf,
                        row_obj.get(&tsf.name),
                        &t_qualified,
                        &row_data,
                        table,
                        errors,
                    );
                }
            }
        }
    }
}

/// Validate a single leaf sub-field inside an array/blocks row container (Group, Row,
/// Collapsible, or Tabs). Runs the required check, date format check, and custom Lua
/// validate function — the same three-step sequence shared by all four container types.
fn validate_leaf_sub_field(
    lua: &Lua,
    sf: &FieldDefinition,
    value: Option<&Value>,
    qualified_name: &str,
    row_data: &HashMap<String, Value>,
    table: &str,
    errors: &mut Vec<FieldError>,
) {
    let is_empty = match value {
        None => true,
        Some(Value::Null) => true,
        Some(Value::String(s)) => s.is_empty(),
        _ => false,
    };

    // 1. Required check (skip for Checkbox — absent/false is valid)
    if sf.required && is_empty && sf.field_type != FieldType::Checkbox {
        errors.push(FieldError::with_key(
            qualified_name.to_owned(),
            format!("{} is required", sf.name),
            "validation.required",
            HashMap::from([("field".to_string(), sf.name.clone())]),
        ));
    }

    // 2. Date format check
    if sf.field_type == FieldType::Date
        && !is_empty
        && let Some(Value::String(s)) = value
        && !is_valid_date_format(s)
    {
        errors.push(FieldError::with_key(
            qualified_name.to_owned(),
            format!("{} is not a valid date format", sf.name),
            "validation.invalid_date",
            HashMap::from([("field".to_string(), sf.name.clone())]),
        ));
    }

    // 3. Custom Lua validate function
    if let Some(ref validate_ref) = sf.validate
        && let Some(val) = value
    {
        match run_validate_function_inner(lua, validate_ref, val, row_data, table, &sf.name) {
            Ok(Some(err_msg)) => {
                errors.push(FieldError::new(qualified_name.to_owned(), err_msg));
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!("Validate function '{}' error: {}", validate_ref, e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::core::field::{BlockDefinition, FieldDefinition, FieldTab, FieldType};
    use crate::hooks::lifecycle::validation::{ValidationCtx, validate_fields_inner};
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn test_validate_array_sub_field_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("label", FieldType::Text)
                        .required(true)
                        .build(),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!([{"label": ""}]));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx {
                conn: &conn,
                table: "test",
                exclude_id: None,
                is_draft: false,
                locale_ctx: None,
            },
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.errors[0].field.contains("items[0][label]"));
    }

    #[test]
    fn test_validate_blocks_sub_field_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("content", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new(
                    "text",
                    vec![
                        FieldDefinition::builder("body", FieldType::Text)
                            .required(true)
                            .build(),
                    ],
                )])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert(
            "content".to_string(),
            json!([{"_block_type": "text", "body": ""}]),
        );
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx {
                conn: &conn,
                table: "test",
                exclude_id: None,
                is_draft: false,
                locale_ctx: None,
            },
        );
        assert!(result.is_err());
        assert!(
            result.unwrap_err().errors[0]
                .field
                .contains("content[0][body]")
        );
    }

    #[test]
    fn test_validate_nested_array_in_array() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("outer", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("inner", FieldType::Array)
                        .fields(vec![
                            FieldDefinition::builder("value", FieldType::Text)
                                .required(true)
                                .build(),
                        ])
                        .build(),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert(
            "outer".to_string(),
            json!([
                {"inner": [{"value": ""}]}
            ]),
        );
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx {
                conn: &conn,
                table: "test",
                exclude_id: None,
                is_draft: false,
                locale_ctx: None,
            },
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.errors[0].field.contains("outer[0][inner][0][value]"));
    }

    #[test]
    fn test_validate_group_inside_array() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("meta", FieldType::Group)
                        .fields(vec![
                            FieldDefinition::builder("title", FieldType::Text)
                                .required(true)
                                .build(),
                        ])
                        .build(),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert(
            "items".to_string(),
            json!([
                {"meta__title": ""}
            ]),
        );
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx {
                conn: &conn,
                table: "test",
                exclude_id: None,
                is_draft: false,
                locale_ctx: None,
            },
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.errors[0].field.contains("items[0][meta__title]"));
    }

    #[test]
    fn test_validate_date_inside_array_subfield() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("events", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("date", FieldType::Date).build(),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert(
            "events".to_string(),
            json!([
                {"date": "not-a-date"}
            ]),
        );
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx {
                conn: &conn,
                table: "test",
                exclude_id: None,
                is_draft: false,
                locale_ctx: None,
            },
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("valid date"));
    }

    #[test]
    fn test_validate_custom_validate_in_array_subfield() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"
            package.loaded["validators"] = package.loaded["validators"] or {}
            package.loaded["validators"].validate_sub = function(value, ctx)

                if value == "invalid" then

                    return "sub-field invalid"
                end

                return true
            end
        "#,
        )
        .exec()
        .unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("val", FieldType::Text)
                        .validate("validators.validate_sub")
                        .build(),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert(
            "items".to_string(),
            json!([
                {"val": "invalid"}
            ]),
        );
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx {
                conn: &conn,
                table: "test",
                exclude_id: None,
                is_draft: false,
                locale_ctx: None,
            },
        );
        assert!(result.is_err());
        assert!(
            result.unwrap_err().errors[0]
                .message
                .contains("sub-field invalid")
        );
    }

    #[test]
    fn test_validate_date_in_group_inside_array() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("meta", FieldType::Group)
                        .fields(vec![
                            FieldDefinition::builder("publish_date", FieldType::Date).build(),
                        ])
                        .build(),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert(
            "items".to_string(),
            json!([
                {"meta__publish_date": "bad-date"}
            ]),
        );
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx {
                conn: &conn,
                table: "test",
                exclude_id: None,
                is_draft: false,
                locale_ctx: None,
            },
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("valid date"));
    }

    #[test]
    fn test_validate_custom_function_in_group_inside_array() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"
            package.loaded["validators"] = package.loaded["validators"] or {}
            package.loaded["validators"].validate_group_sub = function(value, ctx)

                return "group validation error"
            end
        "#,
        )
        .exec()
        .unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("meta", FieldType::Group)
                        .fields(vec![
                            FieldDefinition::builder("slug", FieldType::Text)
                                .validate("validators.validate_group_sub")
                                .build(),
                        ])
                        .build(),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert(
            "items".to_string(),
            json!([
                {"meta__slug": "test-slug"}
            ]),
        );
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx {
                conn: &conn,
                table: "test",
                exclude_id: None,
                is_draft: false,
                locale_ctx: None,
            },
        );
        assert!(result.is_err());
        assert!(
            result.unwrap_err().errors[0]
                .message
                .contains("group validation error")
        );
    }

    #[test]
    fn test_validate_array_sub_field_skipped_for_draft() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("label", FieldType::Text)
                        .required(true)
                        .build(),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!([{"label": ""}]));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx {
                conn: &conn,
                table: "test",
                exclude_id: None,
                is_draft: true,
                locale_ctx: None,
            },
        );
        assert!(
            result.is_ok(),
            "Array sub-field required check should be skipped for drafts"
        );
    }

    #[test]
    fn test_validate_blocks_unknown_block_type_skipped() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("content", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new(
                    "text",
                    vec![
                        FieldDefinition::builder("body", FieldType::Text)
                            .required(true)
                            .build(),
                    ],
                )])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert(
            "content".to_string(),
            json!([{"_block_type": "image", "url": ""}]),
        );
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx {
                conn: &conn,
                table: "test",
                exclude_id: None,
                is_draft: false,
                locale_ctx: None,
            },
        );
        assert!(
            result.is_ok(),
            "Unknown block type rows should be silently skipped"
        );
    }

    #[test]
    fn test_validate_array_non_object_rows_skipped() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("label", FieldType::Text)
                        .required(true)
                        .build(),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!(["plain-string", 42, null]));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx {
                conn: &conn,
                table: "test",
                exclude_id: None,
                is_draft: false,
                locale_ctx: None,
            },
        );
        assert!(
            result.is_ok(),
            "Non-object array rows should be silently skipped"
        );
    }

    #[test]
    fn test_validate_collapsible_inside_array_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("details", FieldType::Collapsible)
                        .fields(vec![
                            FieldDefinition::builder("note", FieldType::Text)
                                .required(true)
                                .build(),
                        ])
                        .build(),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!([{"note": ""}]));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx {
                conn: &conn,
                table: "test",
                exclude_id: None,
                is_draft: false,
                locale_ctx: None,
            },
        );
        assert!(
            result.is_err(),
            "Collapsible sub-field inside array should be validated"
        );
        assert!(
            result.unwrap_err().errors[0]
                .field
                .contains("items[0][note]")
        );
    }

    #[test]
    fn test_validate_collapsible_inside_array_date_invalid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("events", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("info", FieldType::Collapsible)
                        .fields(vec![
                            FieldDefinition::builder("start", FieldType::Date).build(),
                        ])
                        .build(),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("events".to_string(), json!([{"start": "not-a-date"}]));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx {
                conn: &conn,
                table: "test",
                exclude_id: None,
                is_draft: false,
                locale_ctx: None,
            },
        );
        assert!(
            result.is_err(),
            "Invalid date inside collapsible in array should fail"
        );
        assert!(result.unwrap_err().errors[0].message.contains("valid date"));
    }

    #[test]
    fn test_validate_tabs_inside_array_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("layout", FieldType::Tabs)
                        .tabs(vec![FieldTab::new(
                            "Content",
                            vec![
                                FieldDefinition::builder("title", FieldType::Text)
                                    .required(true)
                                    .build(),
                            ],
                        )])
                        .build(),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!([{"title": ""}]));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx {
                conn: &conn,
                table: "test",
                exclude_id: None,
                is_draft: false,
                locale_ctx: None,
            },
        );
        assert!(
            result.is_err(),
            "Required field inside tabs inside array should be validated"
        );
        assert!(
            result.unwrap_err().errors[0]
                .field
                .contains("items[0][title]")
        );
    }

    #[test]
    fn test_validate_tabs_inside_array_date_invalid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("layout", FieldType::Tabs)
                        .tabs(vec![FieldTab::new(
                            "Meta",
                            vec![FieldDefinition::builder("pub_date", FieldType::Date).build()],
                        )])
                        .build(),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!([{"pub_date": "bad-date"}]));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx {
                conn: &conn,
                table: "test",
                exclude_id: None,
                is_draft: false,
                locale_ctx: None,
            },
        );
        assert!(
            result.is_err(),
            "Invalid date inside tabs inside array should fail"
        );
        assert!(result.unwrap_err().errors[0].message.contains("valid date"));
    }

    #[test]
    fn test_validate_tabs_inside_array_custom_validate() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"
            package.loaded["validators"] = {
                validate_tab_row = function(value, ctx)

                    if value == "bad" then return "tab field error" end

                    return true
                end
            }
        "#,
        )
        .exec()
        .unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("layout", FieldType::Tabs)
                        .tabs(vec![FieldTab::new(
                            "Content",
                            vec![
                                FieldDefinition::builder("slug", FieldType::Text)
                                    .validate("validators.validate_tab_row")
                                    .build(),
                            ],
                        )])
                        .build(),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!([{"slug": "bad"}]));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx {
                conn: &conn,
                table: "test",
                exclude_id: None,
                is_draft: false,
                locale_ctx: None,
            },
        );
        assert!(
            result.is_err(),
            "Custom validator inside tabs inside array should fire"
        );
        assert!(
            result.unwrap_err().errors[0]
                .message
                .contains("tab field error")
        );
    }

    #[test]
    fn test_validate_row_inside_array_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("row", FieldType::Row)
                        .fields(vec![
                            FieldDefinition::builder("label", FieldType::Text)
                                .required(true)
                                .build(),
                        ])
                        .build(),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!([{"label": ""}]));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx {
                conn: &conn,
                table: "test",
                exclude_id: None,
                is_draft: false,
                locale_ctx: None,
            },
        );
        assert!(
            result.is_err(),
            "Required field inside row inside array should be validated"
        );
        assert!(
            result.unwrap_err().errors[0]
                .field
                .contains("items[0][label]")
        );
    }

    #[test]
    fn test_validate_row_inside_array_date_invalid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("r", FieldType::Row)
                        .fields(vec![
                            FieldDefinition::builder("event_date", FieldType::Date).build(),
                        ])
                        .build(),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!([{"event_date": "not-a-date"}]));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx {
                conn: &conn,
                table: "test",
                exclude_id: None,
                is_draft: false,
                locale_ctx: None,
            },
        );
        assert!(
            result.is_err(),
            "Invalid date inside row inside array should fail"
        );
        assert!(result.unwrap_err().errors[0].message.contains("valid date"));
    }

    #[test]
    fn test_validate_row_inside_array_custom_validate() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"
            package.loaded["validators"] = {
                validate_row_field = function(value, ctx)

                    if value == "forbidden" then return "row field forbidden" end

                    return true
                end
            }
        "#,
        )
        .exec()
        .unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("r", FieldType::Row)
                        .fields(vec![
                            FieldDefinition::builder("code", FieldType::Text)
                                .validate("validators.validate_row_field")
                                .build(),
                        ])
                        .build(),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!([{"code": "forbidden"}]));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx {
                conn: &conn,
                table: "test",
                exclude_id: None,
                is_draft: false,
                locale_ctx: None,
            },
        );
        assert!(
            result.is_err(),
            "Custom validator inside row inside array should fire"
        );
        assert!(
            result.unwrap_err().errors[0]
                .message
                .contains("row field forbidden")
        );
    }

    #[test]
    fn test_validate_blocks_inside_array_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("outer", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("sections", FieldType::Blocks)
                        .blocks(vec![BlockDefinition::new(
                            "heading",
                            vec![
                                FieldDefinition::builder("text", FieldType::Text)
                                    .required(true)
                                    .build(),
                            ],
                        )])
                        .build(),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert(
            "outer".to_string(),
            json!([
                {"sections": [{"_block_type": "heading", "text": ""}]}
            ]),
        );
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx {
                conn: &conn,
                table: "test",
                exclude_id: None,
                is_draft: false,
                locale_ctx: None,
            },
        );
        assert!(
            result.is_err(),
            "Required field inside blocks inside array should be validated"
        );
        assert!(
            result.unwrap_err().errors[0]
                .field
                .contains("outer[0][sections][0][text]")
        );
    }

    #[test]
    fn test_validate_collapsible_inside_array_custom_validate() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"
            package.loaded["validators"] = {
                validate_coll_field = function(value, ctx)

                    if value == "nope" then return "collapsible field rejected" end

                    return true
                end
            }
        "#,
        )
        .exec()
        .unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("section", FieldType::Collapsible)
                        .fields(vec![
                            FieldDefinition::builder("val", FieldType::Text)
                                .validate("validators.validate_coll_field")
                                .build(),
                        ])
                        .build(),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!([{"val": "nope"}]));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx {
                conn: &conn,
                table: "test",
                exclude_id: None,
                is_draft: false,
                locale_ctx: None,
            },
        );
        assert!(
            result.is_err(),
            "Custom validator inside collapsible inside array should fire"
        );
        assert!(
            result.unwrap_err().errors[0]
                .message
                .contains("collapsible field rejected")
        );
    }

    #[test]
    fn test_validate_checkbox_inside_array_not_required_when_absent() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("active", FieldType::Checkbox)
                        .required(true)
                        .build(),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!([{}]));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx {
                conn: &conn,
                table: "test",
                exclude_id: None,
                is_draft: false,
                locale_ctx: None,
            },
        );
        assert!(
            result.is_ok(),
            "Checkbox inside array should not be required even when required=true"
        );
    }
}
