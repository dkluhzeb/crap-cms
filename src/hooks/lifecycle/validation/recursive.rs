use std::collections::HashMap;

use mlua::Lua;
use serde_json::Value;

use crate::{
    core::{FieldDefinition, FieldType, validate::FieldError},
    db::{LocaleMode, query::sanitize_locale},
    hooks::ValidationCtx,
};

use super::{
    checks,
    richtext_attrs::{RichtextValidationCtx, validate_richtext_node_attrs},
    sub_fields::{SubFieldParams, validate_sub_fields_inner},
};

/// Recursive validation with prefix support for arbitrary nesting.
/// Group accumulates prefix (`group__`), Row/Collapsible/Tabs pass through.
/// `inherited_localized` tracks locale state for unique checks.
pub(super) fn validate_fields_recursive(
    lua: &Lua,
    fields: &[FieldDefinition],
    data: &HashMap<String, Value>,
    ctx: &ValidationCtx,
    prefix: &str,
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
                    ctx,
                    &new_prefix,
                    inherited_localized || field.localized,
                    errors,
                );
            }
            FieldType::Row | FieldType::Collapsible => {
                validate_fields_recursive(
                    lua,
                    &field.fields,
                    data,
                    ctx,
                    prefix,
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
                        ctx,
                        prefix,
                        inherited_localized,
                        errors,
                    );
                }
            }
            FieldType::Join => {
                // Virtual field — no data to validate
            }
            _ => {
                validate_scalar_field(lua, field, data, ctx, prefix, inherited_localized, errors);
            }
        }
    }
}

/// Validate a single scalar field (not Group/Row/Collapsible/Tabs).
/// Dispatches to individual check functions in `checks` module.
fn validate_scalar_field(
    lua: &Lua,
    field: &FieldDefinition,
    data: &HashMap<String, Value>,
    ctx: &ValidationCtx,
    prefix: &str,
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
        Some(Value::Null) => true,
        Some(Value::String(s)) => s.is_empty(),
        _ => false,
    };
    let is_update = ctx.exclude_id.is_some();

    checks::check_required(
        field,
        &data_key,
        value,
        is_empty,
        ctx.is_draft,
        is_update,
        errors,
    );
    checks::check_row_bounds(field, &data_key, value, ctx.is_draft, errors);

    // Validate sub-fields within Array/Blocks rows
    if !ctx.is_draft
        && matches!(field.field_type, FieldType::Array | FieldType::Blocks)
        && let Some(Value::Array(rows)) = value
    {
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
            let params = SubFieldParams {
                lua,
                parent_name: &data_key,
                idx,
                table: ctx.table,
                registry: ctx.registry,
                is_draft: ctx.is_draft,
            };
            validate_sub_fields_inner(&params, sub_fields, row_obj, errors);
        }
    }

    // Compute the actual DB column name for the unique check.
    // Localized fields store data in suffixed columns (e.g., slug__en).
    let is_localized = (inherited_localized || field.localized) && ctx.locale_ctx.is_some();
    let col_name = if is_localized {
        let lctx = ctx.locale_ctx.unwrap();
        let locale = match &lctx.mode {
            LocaleMode::Single(l) => l.as_str(),
            _ => lctx.config.default_locale.as_str(),
        };
        match sanitize_locale(locale) {
            Ok(l) => format!("{}__{}", data_key, l),
            Err(_) => data_key.clone(),
        }
    } else {
        data_key.clone()
    };

    checks::check_unique(field, &data_key, &col_name, value, is_empty, ctx, errors);
    checks::check_length_bounds(field, &data_key, value, is_empty, errors);
    checks::check_numeric_bounds(field, &data_key, value, is_empty, errors);
    checks::check_email_format(field, &data_key, value, is_empty, errors);
    checks::check_option_valid(field, &data_key, value, is_empty, errors);
    checks::check_has_many_elements(field, &data_key, value, is_empty, errors);
    checks::check_date_field(field, &data_key, value, is_empty, errors);
    checks::check_custom_validate(lua, field, &data_key, value, data, ctx.table, errors);

    // Validate custom node attrs within richtext content
    if field.field_type == FieldType::Richtext
        && !is_empty
        && !field.admin.nodes.is_empty()
        && let Some(registry) = ctx.registry
        && let Some(Value::String(content)) = value
    {
        validate_richtext_node_attrs(
            &RichtextValidationCtx::builder(lua, registry, ctx.table)
                .draft(ctx.is_draft)
                .build(),
            content,
            &data_key,
            field,
            errors,
        );
    }
}

#[cfg(test)]
mod tests {
    use crate::core::field::{FieldAdmin, FieldDefinition, FieldTab, FieldType, JoinConfig};
    use crate::core::registry::Registry;
    use crate::core::richtext::RichtextNodeDef;
    use crate::db::InMemoryConn;
    use crate::hooks::lifecycle::validation::{ValidationCtx, validate_fields_inner};
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn test_validate_group_subfield_required() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, seo__title TEXT)");
        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("title", FieldType::Text)
                        .required(true)
                        .build(),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("seo__title".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.errors[0].field, "seo__title");
    }

    #[test]
    fn test_validate_required_inside_collapsible() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, notes TEXT)");
        let fields = vec![
            FieldDefinition::builder("extra", FieldType::Collapsible)
                .fields(vec![
                    FieldDefinition::builder("notes", FieldType::Text)
                        .required(true)
                        .build(),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("notes".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().errors[0].field, "notes");
    }

    #[test]
    fn test_validate_required_inside_tabs() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, body TEXT)");
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "Content",
                    vec![
                        FieldDefinition::builder("body", FieldType::Text)
                            .required(true)
                            .build(),
                    ],
                )])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("body".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().errors[0].field, "body");
    }

    #[test]
    fn test_validate_group_inside_tabs_required() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, seo__title TEXT)");
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "SEO",
                    vec![
                        FieldDefinition::builder("seo", FieldType::Group)
                            .fields(vec![
                                FieldDefinition::builder("title", FieldType::Text)
                                    .required(true)
                                    .build(),
                            ])
                            .build(),
                    ],
                )])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("seo__title".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().errors[0].field, "seo__title");
    }

    #[test]
    fn test_validate_group_inside_collapsible_required() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, seo__title TEXT)");
        let fields = vec![
            FieldDefinition::builder("extra", FieldType::Collapsible)
                .fields(vec![
                    FieldDefinition::builder("seo", FieldType::Group)
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
        data.insert("seo__title".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().errors[0].field, "seo__title");
    }

    #[test]
    fn test_validate_date_inside_tabs() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, publish_date TEXT)");
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "Meta",
                    vec![FieldDefinition::builder("publish_date", FieldType::Date).build()],
                )])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("publish_date".to_string(), json!("not-a-date"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("valid date"));
    }

    #[test]
    fn test_validate_unique_inside_tabs() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup(
            "CREATE TABLE test (id TEXT PRIMARY KEY, slug TEXT);
             INSERT INTO test (id, slug) VALUES ('existing', 'taken');",
        );
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "Meta",
                    vec![
                        FieldDefinition::builder("slug", FieldType::Text)
                            .unique(true)
                            .build(),
                    ],
                )])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("slug".to_string(), json!("taken"));
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
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, body TEXT)");
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "Content",
                    vec![
                        FieldDefinition::builder("body", FieldType::Text)
                            .validate("validators.validate_tabs_field")
                            .build(),
                    ],
                )])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("body".to_string(), json!("bad"));
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
                .contains("tabs validation error")
        );
    }

    #[test]
    fn test_validate_deeply_nested_tabs_collapsible_group() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, og__title TEXT)");
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "Advanced",
                    vec![
                        FieldDefinition::builder("advanced", FieldType::Collapsible)
                            .fields(vec![
                                FieldDefinition::builder("og", FieldType::Group)
                                    .fields(vec![
                                        FieldDefinition::builder("title", FieldType::Text)
                                            .required(true)
                                            .build(),
                                    ])
                                    .build(),
                            ])
                            .build(),
                    ],
                )])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("og__title".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "Deeply nested Group inside Collapsible inside Tabs should validate"
        );
        assert_eq!(result.unwrap_err().errors[0].field, "og__title");
    }

    #[test]
    fn test_validate_layout_fields_skipped_for_drafts() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, body TEXT)");
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "Content",
                    vec![
                        FieldDefinition::builder("body", FieldType::Text)
                            .required(true)
                            .build(),
                    ],
                )])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("body".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").draft(true).build(),
        );
        assert!(
            result.is_ok(),
            "Draft saves should skip required checks in layout fields"
        );
    }

    // ── Group containing layout fields ─────

    #[test]
    fn test_validate_group_containing_row_required() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, meta__title TEXT)");
        let fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("r", FieldType::Row)
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
        data.insert("meta__title".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err(), "Group→Row: required field should fail");
        assert_eq!(result.unwrap_err().errors[0].field, "meta__title");
    }

    #[test]
    fn test_validate_group_containing_collapsible_required() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, seo__robots TEXT)");
        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("c", FieldType::Collapsible)
                        .fields(vec![
                            FieldDefinition::builder("robots", FieldType::Text)
                                .required(true)
                                .build(),
                        ])
                        .build(),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("seo__robots".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "Group→Collapsible: required field should fail"
        );
        assert_eq!(result.unwrap_err().errors[0].field, "seo__robots");
    }

    #[test]
    fn test_validate_group_containing_tabs_required() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, settings__theme TEXT)");
        let fields = vec![
            FieldDefinition::builder("settings", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("t", FieldType::Tabs)
                        .tabs(vec![FieldTab::new(
                            "General",
                            vec![
                                FieldDefinition::builder("theme", FieldType::Text)
                                    .required(true)
                                    .build(),
                            ],
                        )])
                        .build(),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("settings__theme".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err(), "Group→Tabs: required field should fail");
        assert_eq!(result.unwrap_err().errors[0].field, "settings__theme");
    }

    #[test]
    fn test_validate_group_tabs_group_three_levels_required() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, outer__inner__deep TEXT)");
        let fields = vec![
            FieldDefinition::builder("outer", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("t", FieldType::Tabs)
                        .tabs(vec![FieldTab::new(
                            "Tab",
                            vec![
                                FieldDefinition::builder("inner", FieldType::Group)
                                    .fields(vec![
                                        FieldDefinition::builder("deep", FieldType::Text)
                                            .required(true)
                                            .build(),
                                    ])
                                    .build(),
                            ],
                        )])
                        .build(),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("outer__inner__deep".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "Group→Tabs→Group: required field should fail"
        );
        assert_eq!(result.unwrap_err().errors[0].field, "outer__inner__deep");
    }

    #[test]
    fn test_validate_group_containing_tabs_unique() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup(
            "CREATE TABLE test (id TEXT PRIMARY KEY, config__slug TEXT);
             INSERT INTO test (id, config__slug) VALUES ('existing', 'taken');",
        );
        let fields = vec![
            FieldDefinition::builder("config", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("t", FieldType::Tabs)
                        .tabs(vec![FieldTab::new(
                            "Tab",
                            vec![
                                FieldDefinition::builder("slug", FieldType::Text)
                                    .unique(true)
                                    .build(),
                            ],
                        )])
                        .build(),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("config__slug".to_string(), json!("taken"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "Group→Tabs: unique field should fail on duplicate"
        );
        assert_eq!(result.unwrap_err().errors[0].field, "config__slug");
    }

    #[test]
    fn test_validate_group_containing_row_date_format() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, meta__date TEXT)");
        let fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("r", FieldType::Row)
                        .fields(vec![
                            FieldDefinition::builder("date", FieldType::Date).build(),
                        ])
                        .build(),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("meta__date".to_string(), json!("not-a-date"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err(), "Group→Row: invalid date should fail");
        assert_eq!(result.unwrap_err().errors[0].field, "meta__date");
    }

    #[test]
    fn test_validate_group_containing_row_valid_passes() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, meta__title TEXT)");
        let fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("r", FieldType::Row)
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
        data.insert("meta__title".to_string(), json!("Valid Title"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok(), "Group→Row: valid data should pass");
    }

    #[test]
    fn join_field_skipped_in_validation() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY)");
        let fields = vec![
            FieldDefinition::builder("posts", FieldType::Join)
                .required(true)
                .join(JoinConfig::new("posts", "author"))
                .build(),
        ];
        let data = HashMap::new();
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_ok(),
            "Join field should be skipped entirely during validation"
        );
    }

    #[test]
    fn test_validate_nested_group_in_group_prefix() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, outer__inner__field TEXT)");
        let fields = vec![
            FieldDefinition::builder("outer", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("inner", FieldType::Group)
                        .fields(vec![
                            FieldDefinition::builder("field", FieldType::Text)
                                .required(true)
                                .build(),
                        ])
                        .build(),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("outer__inner__field".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "Nested group prefix should be outer__inner__field"
        );
        assert_eq!(result.unwrap_err().errors[0].field, "outer__inner__field");
    }

    #[test]
    fn test_validate_date_inside_collapsible_top_level() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, pub_date TEXT)");
        let fields = vec![
            FieldDefinition::builder("extra", FieldType::Collapsible)
                .fields(vec![
                    FieldDefinition::builder("pub_date", FieldType::Date).build(),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("pub_date".to_string(), json!("not-a-date"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "Invalid date inside collapsible at top-level should fail"
        );
        assert!(result.unwrap_err().errors[0].message.contains("valid date"));
    }

    #[test]
    fn test_validate_date_inside_row_top_level() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, event_date TEXT)");
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Row)
                .fields(vec![
                    FieldDefinition::builder("event_date", FieldType::Date).build(),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("event_date".to_string(), json!("not-a-date"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "Invalid date inside row at top-level should fail"
        );
        assert!(result.unwrap_err().errors[0].message.contains("valid date"));
    }

    // --- Richtext node attr validation integration tests ---

    #[test]
    fn test_richtext_node_attr_required_through_validation_pipeline() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE pages (id TEXT PRIMARY KEY, content TEXT)");

        let mut reg = Registry::new();
        reg.register_richtext_node(
            RichtextNodeDef::builder("cta", "CTA")
                .attrs(vec![
                    FieldDefinition::builder("text", FieldType::Text)
                        .required(true)
                        .build(),
                    FieldDefinition::builder("url", FieldType::Text)
                        .required(true)
                        .build(),
                ])
                .build(),
        );

        let fields = vec![
            FieldDefinition::builder("content", FieldType::Richtext)
                .admin(
                    FieldAdmin::builder()
                        .nodes(vec!["cta".to_string()])
                        .richtext_format("json")
                        .build(),
                )
                .build(),
        ];

        let json_content =
            r#"{"type":"doc","content":[{"type":"cta","attrs":{"text":"","url":""}}]}"#;
        let mut data = HashMap::new();
        data.insert("content".to_string(), json!(json_content));

        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "pages")
                .registry(&reg)
                .build(),
        );

        assert!(result.is_err(), "empty required node attrs should fail");
        let errs = result.unwrap_err().errors;
        assert_eq!(errs.len(), 2);
        assert_eq!(errs[0].field, "content[cta#0].text");
        assert_eq!(errs[1].field, "content[cta#0].url");
    }

    #[test]
    fn test_richtext_node_attr_valid_passes_pipeline() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE pages (id TEXT PRIMARY KEY, content TEXT)");

        let mut reg = Registry::new();
        reg.register_richtext_node(
            RichtextNodeDef::builder("cta", "CTA")
                .attrs(vec![
                    FieldDefinition::builder("text", FieldType::Text)
                        .required(true)
                        .build(),
                ])
                .build(),
        );

        let fields = vec![
            FieldDefinition::builder("content", FieldType::Richtext)
                .admin(
                    FieldAdmin::builder()
                        .nodes(vec!["cta".to_string()])
                        .richtext_format("json")
                        .build(),
                )
                .build(),
        ];

        let json_content =
            r#"{"type":"doc","content":[{"type":"cta","attrs":{"text":"Click me"}}]}"#;
        let mut data = HashMap::new();
        data.insert("content".to_string(), json!(json_content));

        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "pages")
                .registry(&reg)
                .build(),
        );

        assert!(result.is_ok(), "valid node attrs should pass");
    }

    #[test]
    fn test_richtext_node_attr_no_registry_skips_validation() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE pages (id TEXT PRIMARY KEY, content TEXT)");

        let fields = vec![
            FieldDefinition::builder("content", FieldType::Richtext)
                .admin(
                    FieldAdmin::builder()
                        .nodes(vec!["cta".to_string()])
                        .richtext_format("json")
                        .build(),
                )
                .build(),
        ];

        // Content with invalid data, but no registry provided
        let json_content = r#"{"type":"doc","content":[{"type":"cta","attrs":{"text":""}}]}"#;
        let mut data = HashMap::new();
        data.insert("content".to_string(), json!(json_content));

        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "pages").build(), // no registry
        );

        assert!(
            result.is_ok(),
            "without registry, node attr validation is skipped"
        );
    }

    #[test]
    fn test_richtext_node_attrs_alongside_regular_field_errors() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE pages (id TEXT PRIMARY KEY, title TEXT, content TEXT)");

        let mut reg = Registry::new();
        reg.register_richtext_node(
            RichtextNodeDef::builder("cta", "CTA")
                .attrs(vec![
                    FieldDefinition::builder("text", FieldType::Text)
                        .required(true)
                        .build(),
                ])
                .build(),
        );

        let fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .required(true)
                .build(),
            FieldDefinition::builder("content", FieldType::Richtext)
                .admin(
                    FieldAdmin::builder()
                        .nodes(vec!["cta".to_string()])
                        .richtext_format("json")
                        .build(),
                )
                .build(),
        ];

        let json_content = r#"{"type":"doc","content":[{"type":"cta","attrs":{"text":""}}]}"#;
        let mut data = HashMap::new();
        data.insert("title".to_string(), json!(""));
        data.insert("content".to_string(), json!(json_content));

        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "pages")
                .registry(&reg)
                .build(),
        );

        assert!(result.is_err());
        let errs = result.unwrap_err().errors;
        assert_eq!(errs.len(), 2);
        // Regular field error first, then node attr error
        assert_eq!(errs[0].field, "title");
        assert_eq!(errs[1].field, "content[cta#0].text");
    }
}
