//! Parsing functions for field definitions from Lua tables.

use anyhow::{Result, anyhow, bail};
use mlua::{Table, Value};
use serde_json::{Number as JsonNumber, Value as JsonValue};

use crate::{
    core::field::{
        FieldAccess, FieldAdmin, FieldDefinition, FieldHooks, FieldType, JoinConfig, McpFieldConfig,
    },
    db::query,
};

use super::{
    admin::parse_field_admin,
    blocks::{parse_block_definitions, parse_tab_definitions},
    helpers::*,
    relationship::parse_field_relationship,
};

/// Parse a Lua sequence of field tables into a `Vec<FieldDefinition>`.
pub(crate) fn parse_fields(fields_tbl: &Table) -> Result<Vec<FieldDefinition>> {
    let mut fields = Vec::new();

    for pair in fields_tbl.clone().sequence_values::<Table>() {
        let field_tbl = pair?;
        let name: String =
            get_string_val(&field_tbl, "name").map_err(|_| anyhow!("Field missing 'name'"))?;

        if !query::is_valid_identifier(&name) {
            bail!(
                "Invalid field name '{}' — use alphanumeric and underscores only",
                name
            );
        }

        let type_str: String =
            get_string_val(&field_tbl, "type").unwrap_or_else(|_| "text".to_string());
        let field_type = FieldType::from_str(&type_str);

        let required = get_bool(&field_tbl, "required", false);
        let unique = get_bool(&field_tbl, "unique", false);
        let index = get_bool(&field_tbl, "index", false);
        let validate = get_string(&field_tbl, "validate");

        let default_value = {
            let val: Value = field_tbl.get("default_value").unwrap_or(Value::Nil);
            match val {
                Value::Nil => None,
                Value::Boolean(b) => Some(JsonValue::Bool(b)),
                Value::Integer(i) => Some(JsonValue::Number(JsonNumber::from(i))),
                Value::Number(n) => JsonNumber::from_f64(n).map(JsonValue::Number),
                Value::String(s) => Some(JsonValue::String(s.to_str()?.to_string())),
                _ => None,
            }
        };

        let options = if let Ok(opts_tbl) = get_table(&field_tbl, "options") {
            parse_select_options(&opts_tbl)?
        } else {
            Vec::new()
        };

        let admin = if let Ok(admin_tbl) = get_table(&field_tbl, "admin") {
            parse_field_admin(&admin_tbl)?
        } else {
            FieldAdmin::default()
        };

        let hooks = if let Ok(hooks_tbl) = get_table(&field_tbl, "hooks") {
            parse_field_hooks(&hooks_tbl)?
        } else {
            FieldHooks::default()
        };

        let access = if let Ok(access_tbl) = get_table(&field_tbl, "access") {
            parse_field_access(&access_tbl)
        } else {
            FieldAccess::default()
        };

        // Parse relationship config
        let relationship = parse_field_relationship(&field_tbl, &field_type)?;

        // Parse sub-fields for Array, Group, Row, and Collapsible types (recursive)
        let sub_fields = if field_type == FieldType::Array
            || field_type == FieldType::Group
            || field_type == FieldType::Row
            || field_type == FieldType::Collapsible
        {
            if let Ok(sub_fields_tbl) = get_table(&field_tbl, "fields") {
                parse_fields(&sub_fields_tbl)?
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        let localized = get_bool(&field_tbl, "localized", false);

        // Parse picker_appearance for date fields
        let picker_appearance = if field_type == FieldType::Date {
            get_string(&field_tbl, "picker_appearance")
        } else {
            None
        };

        // Parse block definitions for Blocks type
        let block_defs = if field_type == FieldType::Blocks {
            if let Ok(blocks_tbl) = get_table(&field_tbl, "blocks") {
                parse_block_definitions(&blocks_tbl)?
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        // Parse tab definitions for Tabs type
        let tab_defs = if field_type == FieldType::Tabs {
            if let Ok(tabs_tbl) = get_table(&field_tbl, "tabs") {
                parse_tab_definitions(&tabs_tbl)?
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        let min_rows = field_tbl.get::<Option<usize>>("min_rows").ok().flatten();
        let max_rows = field_tbl.get::<Option<usize>>("max_rows").ok().flatten();
        let min_length = field_tbl.get::<Option<usize>>("min_length").ok().flatten();
        let max_length = field_tbl.get::<Option<usize>>("max_length").ok().flatten();
        let min = match field_tbl.get::<Value>("min") {
            Ok(Value::Number(n)) => Some(n),
            Ok(Value::Integer(i)) => Some(i as f64),
            _ => None,
        };
        let max = match field_tbl.get::<Value>("max") {
            Ok(Value::Number(n)) => Some(n),
            Ok(Value::Integer(i)) => Some(i as f64),
            _ => None,
        };

        let has_many = get_bool(&field_tbl, "has_many", false);
        let min_date = get_string(&field_tbl, "min_date");
        let max_date = get_string(&field_tbl, "max_date");

        // Parse join config for Join fields
        let join = if field_type == FieldType::Join {
            let collection = get_string(&field_tbl, "collection").unwrap_or_default();
            let on = get_string(&field_tbl, "on").unwrap_or_default();
            Some(JoinConfig::new(collection, on))
        } else {
            None
        };

        // Parse MCP config for field
        let mcp = if let Ok(mcp_tbl) = get_table(&field_tbl, "mcp") {
            McpFieldConfig {
                description: get_string(&mcp_tbl, "description"),
            }
        } else {
            Default::default()
        };

        let mut fd_builder = FieldDefinition::builder(&name, field_type)
            .required(required)
            .unique(unique)
            .index(index)
            .admin(admin)
            .hooks(hooks)
            .access(access)
            .mcp(mcp)
            .fields(sub_fields)
            .blocks(block_defs)
            .tabs(tab_defs)
            .localized(localized)
            .has_many(has_many)
            .options(options);

        if let Some(v) = validate {
            fd_builder = fd_builder.validate(v);
        }
        if let Some(v) = default_value {
            fd_builder = fd_builder.default_value(v);
        }
        if let Some(v) = relationship {
            fd_builder = fd_builder.relationship(v);
        }
        if let Some(v) = picker_appearance {
            fd_builder = fd_builder.picker_appearance(v);
        }
        if let Some(v) = min_rows {
            fd_builder = fd_builder.min_rows(v);
        }
        if let Some(v) = max_rows {
            fd_builder = fd_builder.max_rows(v);
        }
        if let Some(v) = min_length {
            fd_builder = fd_builder.min_length(v);
        }
        if let Some(v) = max_length {
            fd_builder = fd_builder.max_length(v);
        }
        if let Some(v) = min {
            fd_builder = fd_builder.min(v);
        }
        if let Some(v) = max {
            fd_builder = fd_builder.max(v);
        }
        if let Some(v) = min_date {
            fd_builder = fd_builder.min_date(v);
        }
        if let Some(v) = max_date {
            fd_builder = fd_builder.max_date(v);
        }
        if let Some(v) = join {
            fd_builder = fd_builder.join(v);
        }
        fields.push(fd_builder.build());
    }

    Ok(fields)
}

pub(super) fn parse_field_access(access_tbl: &Table) -> FieldAccess {
    FieldAccess {
        read: get_string(access_tbl, "read"),
        create: get_string(access_tbl, "create"),
        update: get_string(access_tbl, "update"),
    }
}

fn parse_field_hooks(hooks_tbl: &Table) -> Result<FieldHooks> {
    Ok(FieldHooks {
        before_validate: parse_string_list(hooks_tbl, "before_validate")?,
        before_change: parse_string_list(hooks_tbl, "before_change")?,
        after_change: parse_string_list(hooks_tbl, "after_change")?,
        after_read: parse_string_list(hooks_tbl, "after_read")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Lua;

    #[test]
    fn test_parse_field_access() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("read", "hooks.access.check_role").unwrap();
        tbl.set("create", "hooks.access.admin_only").unwrap();
        let access = parse_field_access(&tbl);
        assert_eq!(access.read.as_deref(), Some("hooks.access.check_role"));
        assert_eq!(access.create.as_deref(), Some("hooks.access.admin_only"));
        assert!(access.update.is_none());
    }

    #[test]
    fn test_parse_field_index() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "status").unwrap();
        field.set("type", "text").unwrap();
        field.set("index", true).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert!(fields[0].index, "index should be true");
    }

    #[test]
    fn test_parse_field_index_default_false() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "title").unwrap();
        field.set("type", "text").unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert!(!fields[0].index, "index should default to false");
    }

    #[test]
    fn test_parse_fields_default_value_boolean() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "active").unwrap();
        field.set("type", "checkbox").unwrap();
        field.set("default_value", true).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert_eq!(fields[0].default_value, Some(JsonValue::Bool(true)));
    }

    #[test]
    fn test_parse_fields_default_value_integer() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "count").unwrap();
        field.set("type", "number").unwrap();
        field.set("default_value", 42i64).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert_eq!(fields[0].default_value, Some(JsonValue::Number(42.into())));
    }

    #[test]
    fn test_parse_fields_default_value_float() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "ratio").unwrap();
        field.set("type", "number").unwrap();
        field.set("default_value", 3.14f64).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        let dv = fields[0].default_value.as_ref().unwrap();
        assert!(dv.is_number());
    }

    #[test]
    fn test_parse_fields_default_value_table_ignored() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "data").unwrap();
        field.set("type", "json").unwrap();
        let inner = lua.create_table().unwrap();
        field.set("default_value", inner).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert!(fields[0].default_value.is_none());
    }

    #[test]
    fn test_parse_fields_group_with_subfields() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "meta").unwrap();
        field.set("type", "group").unwrap();
        let sub = lua.create_table().unwrap();
        let sf = lua.create_table().unwrap();
        sf.set("name", "title").unwrap();
        sf.set("type", "text").unwrap();
        sub.set(1, sf).unwrap();
        field.set("fields", sub).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert_eq!(fields[0].fields.len(), 1);
        assert_eq!(fields[0].fields[0].name, "title");
    }

    #[test]
    fn test_parse_fields_row_with_subfields() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "layout_row").unwrap();
        field.set("type", "row").unwrap();
        let sub = lua.create_table().unwrap();
        let sf = lua.create_table().unwrap();
        sf.set("name", "first_name").unwrap();
        sf.set("type", "text").unwrap();
        sub.set(1, sf).unwrap();
        field.set("fields", sub).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert_eq!(fields[0].fields.len(), 1);
        assert_eq!(fields[0].fields[0].name, "first_name");
    }

    #[test]
    fn test_parse_fields_collapsible_with_subfields() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "advanced").unwrap();
        field.set("type", "collapsible").unwrap();
        let sub = lua.create_table().unwrap();
        let sf = lua.create_table().unwrap();
        sf.set("name", "notes").unwrap();
        sf.set("type", "textarea").unwrap();
        sub.set(1, sf).unwrap();
        field.set("fields", sub).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert_eq!(fields[0].fields.len(), 1);
        assert_eq!(fields[0].fields[0].name, "notes");
    }

    #[test]
    fn test_parse_fields_date_picker_appearance() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "published_at").unwrap();
        field.set("type", "date").unwrap();
        field.set("picker_appearance", "datetime").unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert_eq!(fields[0].picker_appearance.as_deref(), Some("datetime"));
    }

    #[test]
    fn test_parse_fields_non_date_picker_appearance_ignored() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "title").unwrap();
        field.set("type", "text").unwrap();
        field.set("picker_appearance", "datetime").unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert!(fields[0].picker_appearance.is_none());
    }

    #[test]
    fn test_parse_fields_min_max_float() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "score").unwrap();
        field.set("type", "number").unwrap();
        field.set("min", 0.0f64).unwrap();
        field.set("max", 100.0f64).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert_eq!(fields[0].min, Some(0.0));
        assert_eq!(fields[0].max, Some(100.0));
    }

    #[test]
    fn test_parse_fields_min_max_integer() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "qty").unwrap();
        field.set("type", "number").unwrap();
        field.set("min", 1i64).unwrap();
        field.set("max", 99i64).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert_eq!(fields[0].min, Some(1.0));
        assert_eq!(fields[0].max, Some(99.0));
    }

    #[test]
    fn test_parse_fields_join_type() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "authored_posts").unwrap();
        field.set("type", "join").unwrap();
        field.set("collection", "posts").unwrap();
        field.set("on", "author").unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        let join = fields[0].join.as_ref().unwrap();
        assert_eq!(join.collection, "posts");
        assert_eq!(join.on, "author");
    }

    #[test]
    fn test_parse_fields_mcp_config() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "summary").unwrap();
        field.set("type", "text").unwrap();
        let mcp_tbl = lua.create_table().unwrap();
        mcp_tbl.set("description", "A short summary").unwrap();
        field.set("mcp", mcp_tbl).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert_eq!(
            fields[0].mcp.description.as_deref(),
            Some("A short summary")
        );
    }

    #[test]
    fn test_parse_fields_missing_name_returns_error() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("type", "text").unwrap();
        fields_tbl.set(1, field).unwrap();
        let result = parse_fields(&fields_tbl);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("missing 'name'"));
    }

    #[test]
    fn test_parse_fields_array_min_max_rows() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "images").unwrap();
        field.set("type", "array").unwrap();
        field.set("min_rows", 1usize).unwrap();
        field.set("max_rows", 10usize).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert_eq!(fields[0].min_rows, Some(1));
        assert_eq!(fields[0].max_rows, Some(10));
    }

    #[test]
    fn test_parse_fields_text_min_max_length() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "slug").unwrap();
        field.set("type", "text").unwrap();
        field.set("min_length", 3usize).unwrap();
        field.set("max_length", 64usize).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert_eq!(fields[0].min_length, Some(3));
        assert_eq!(fields[0].max_length, Some(64));
    }

    #[test]
    fn test_parse_fields_date_min_max_date() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "birth_date").unwrap();
        field.set("type", "date").unwrap();
        field.set("min_date", "1900-01-01").unwrap();
        field.set("max_date", "2100-12-31").unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert_eq!(fields[0].min_date.as_deref(), Some("1900-01-01"));
        assert_eq!(fields[0].max_date.as_deref(), Some("2100-12-31"));
    }

    #[test]
    fn test_parse_field_hooks_all_events() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "title").unwrap();
        field.set("type", "text").unwrap();
        let hooks_tbl = lua.create_table().unwrap();
        let bv = lua.create_table().unwrap();
        bv.set(1, "hooks.validate_title").unwrap();
        hooks_tbl.set("before_validate", bv).unwrap();
        let bc = lua.create_table().unwrap();
        bc.set(1, "hooks.transform_title").unwrap();
        hooks_tbl.set("before_change", bc).unwrap();
        let ac = lua.create_table().unwrap();
        ac.set(1, "hooks.after_title_change").unwrap();
        hooks_tbl.set("after_change", ac).unwrap();
        let ar = lua.create_table().unwrap();
        ar.set(1, "hooks.format_title").unwrap();
        hooks_tbl.set("after_read", ar).unwrap();
        field.set("hooks", hooks_tbl).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        let hooks = &fields[0].hooks;
        assert_eq!(hooks.before_validate, vec!["hooks.validate_title"]);
        assert_eq!(hooks.before_change, vec!["hooks.transform_title"]);
        assert_eq!(hooks.after_change, vec!["hooks.after_title_change"]);
        assert_eq!(hooks.after_read, vec!["hooks.format_title"]);
    }
}
