//! Per-field parsing: orchestrates the full Lua → `FieldDefinition` conversion
//! for a single field table, plus the small helper parsers (name, sub-fields,
//! access, hooks).

use anyhow::{Result, anyhow, bail};
use mlua::{Lua, Table};

use crate::{
    core::{
        FieldAccess, FieldAdmin, FieldDefinition, FieldHooks, FieldType, JoinConfig, McpFieldConfig,
    },
    db::query,
};

use super::super::admin::parse_field_admin;
use super::super::blocks::{parse_block_definitions, parse_tab_definitions};
use super::super::helpers::{
    get_bool, get_string, get_string_val, get_table, parse_select_options, parse_string_list,
};
use super::super::relationship::parse_field_relationship;
use super::constraints::{parse_constraints, parse_date_config, parse_default_value};
use super::top::parse_fields;

/// Field names that collide with automatically generated columns which are
/// *not* `_`-prefixed: the main-table primary key and timestamps, and the
/// `parent_id` foreign key on array/blocks join tables. (`_`-prefixed system
/// columns — `_status`, `_ref_count`, `_deleted_at`, `_order`, `_locale`,
/// `_block_type`, `_password_hash`, `_mfa_code`, … — are covered by the
/// leading-underscore rule below.)
const RESERVED_FIELD_NAMES: &[&str] = &["id", "parent_id", "created_at", "updated_at"];

fn parse_field_name(field_tbl: &Table) -> Result<String> {
    let name: String =
        get_string_val(field_tbl, "name").map_err(|_| anyhow!("Field missing 'name'"))?;

    if !query::is_valid_identifier(&name) {
        bail!("Invalid field name '{name}' — use alphanumeric and underscores only");
    }

    if name.contains("__") {
        bail!(
            "Field name '{name}' must not contain double underscores — reserved for group field separation"
        );
    }

    if name.starts_with('_') {
        bail!(
            "Field name '{name}' must not start with an underscore — the '_' prefix is reserved for system columns (e.g. _status, _ref_count, _deleted_at)"
        );
    }

    if RESERVED_FIELD_NAMES.contains(&name.as_str()) {
        bail!(
            "Field name '{name}' is reserved — it collides with an automatically generated column (id, parent_id, created_at, updated_at)"
        );
    }

    Ok(name)
}

fn parse_sub_fields(
    lua: &Lua,
    field_tbl: &Table,
    field_type: &FieldType,
) -> Result<Vec<FieldDefinition>> {
    let has_sub = matches!(
        field_type,
        FieldType::Array | FieldType::Group | FieldType::Row | FieldType::Collapsible
    );

    if !has_sub {
        return Ok(Vec::new());
    }

    get_table(field_tbl, "fields").map_or(Ok(Vec::new()), |tbl| parse_fields(lua, &tbl))
}

/// All parsed parts of a Lua field definition, bundled so the
/// parse and assemble phases stay cleanly separated.
struct ParsedFieldParts {
    name: String,
    field_type: FieldType,
    default_value: Option<serde_json::Value>,
    relationship: Option<crate::core::RelationshipConfig>,
    picker_appearance: Option<crate::core::PickerAppearance>,
    timezone: bool,
    default_timezone: Option<String>,
    constraints: super::constraints::Constraints,
    options: Vec<crate::core::SelectOption>,
    admin: FieldAdmin,
    hooks: FieldHooks,
    access: FieldAccess,
    sub_fields: Vec<FieldDefinition>,
    block_defs: Vec<crate::core::BlockDefinition>,
    tab_defs: Vec<crate::core::FieldTab>,
    join: Option<JoinConfig>,
    mcp: McpFieldConfig,
}

pub(super) fn parse_single_field(lua: &Lua, field_tbl: &Table) -> Result<FieldDefinition> {
    let parts = parse_field_parts(lua, field_tbl)?;
    assemble_field_definition(field_tbl, parts)
}

/// Phase 1 — extract every sub-structure from the Lua field table.
fn parse_field_parts(lua: &Lua, field_tbl: &Table) -> Result<ParsedFieldParts> {
    let name = parse_field_name(field_tbl)?;

    let type_str: String = get_string_val(field_tbl, "type").unwrap_or_else(|_| "text".to_string());
    let field_type = FieldType::parse_lossy(&type_str);

    let default_value = parse_default_value(field_tbl, &name, &field_type)?;
    let relationship = parse_field_relationship(field_tbl, &field_type)?;
    let (picker_appearance, timezone, default_timezone) =
        parse_date_config(field_tbl, &name, &field_type)?;
    let constraints = parse_constraints(field_tbl, &name)?;

    let options =
        get_table(field_tbl, "options").map_or(Ok(Vec::new()), |tbl| parse_select_options(&tbl))?;

    let admin = get_table(field_tbl, "admin")
        .map_or(Ok(FieldAdmin::default()), |tbl| parse_field_admin(&tbl))?;

    let hooks = get_table(field_tbl, "hooks")
        .map_or(Ok(FieldHooks::default()), |tbl| parse_field_hooks(&tbl))?;

    let access = get_table(field_tbl, "access")
        .map(|tbl| parse_field_access(&tbl))
        .unwrap_or_default();

    let sub_fields = parse_sub_fields(lua, field_tbl, &field_type)?;
    let block_defs = parse_block_defs(lua, field_tbl, &field_type)?;
    let tab_defs = parse_tab_defs(lua, field_tbl, &field_type)?;
    let join = parse_join(field_tbl, &field_type);
    let mcp = parse_mcp(field_tbl);

    Ok(ParsedFieldParts {
        name,
        field_type,
        default_value,
        relationship,
        picker_appearance,
        timezone,
        default_timezone,
        constraints,
        options,
        admin,
        hooks,
        access,
        sub_fields,
        block_defs,
        tab_defs,
        join,
        mcp,
    })
}

/// Parse the `blocks` sub-table — only meaningful for `FieldType::Blocks`.
fn parse_block_defs(
    lua: &Lua,
    field_tbl: &Table,
    field_type: &FieldType,
) -> Result<Vec<crate::core::BlockDefinition>> {
    if *field_type != FieldType::Blocks {
        return Ok(Vec::new());
    }
    get_table(field_tbl, "blocks").map_or(Ok(Vec::new()), |tbl| parse_block_definitions(lua, &tbl))
}

/// Parse the `tabs` sub-table — only meaningful for `FieldType::Tabs`.
fn parse_tab_defs(
    lua: &Lua,
    field_tbl: &Table,
    field_type: &FieldType,
) -> Result<Vec<crate::core::FieldTab>> {
    if *field_type != FieldType::Tabs {
        return Ok(Vec::new());
    }
    get_table(field_tbl, "tabs").map_or(Ok(Vec::new()), |tbl| parse_tab_definitions(lua, &tbl))
}

/// Build the `JoinConfig` for `FieldType::Join`. Missing `collection`/`on`
/// default to empty strings — schema validation surfaces the error later.
fn parse_join(field_tbl: &Table, field_type: &FieldType) -> Option<JoinConfig> {
    if *field_type != FieldType::Join {
        return None;
    }
    let collection = get_string(field_tbl, "collection").unwrap_or_default();
    let on = get_string(field_tbl, "on").unwrap_or_default();
    Some(JoinConfig::new(collection, on))
}

/// Parse the optional `mcp` sub-table for MCP introspection metadata.
fn parse_mcp(field_tbl: &Table) -> McpFieldConfig {
    get_table(field_tbl, "mcp")
        .map(|tbl| McpFieldConfig {
            description: get_string(&tbl, "description"),
        })
        .unwrap_or_default()
}

/// Phase 2 — fold parsed parts into a [`FieldDefinition`] via the builder.
fn assemble_field_definition(
    field_tbl: &Table,
    parts: ParsedFieldParts,
) -> Result<FieldDefinition> {
    let mut fd_builder = FieldDefinition::builder(&parts.name, parts.field_type)
        .required(get_bool(field_tbl, "required", false)?)
        .unique(get_bool(field_tbl, "unique", false)?)
        .index(get_bool(field_tbl, "index", false)?)
        .admin(parts.admin)
        .hooks(parts.hooks)
        .access(parts.access)
        .mcp(parts.mcp)
        .fields(parts.sub_fields)
        .blocks(parts.block_defs)
        .tabs(parts.tab_defs)
        .localized(get_bool(field_tbl, "localized", false)?)
        .has_many(get_bool(field_tbl, "has_many", false)?)
        .hidden(get_bool(field_tbl, "hidden", false)?)
        .options(parts.options);

    if let Some(v) = get_string(field_tbl, "validate") {
        fd_builder = fd_builder.validate(v);
    }
    if let Some(v) = parts.default_value {
        fd_builder = fd_builder.default_value(v);
    }
    if let Some(v) = parts.relationship {
        fd_builder = fd_builder.relationship(v);
    }
    if let Some(v) = parts.picker_appearance {
        fd_builder = fd_builder.picker_appearance(v);
    }

    fd_builder = apply_constraint_bounds(fd_builder, &parts.constraints);
    fd_builder = apply_date_bounds(fd_builder, field_tbl);

    if parts.timezone {
        fd_builder = fd_builder.timezone(true);
    }
    if let Some(v) = parts.default_timezone {
        fd_builder = fd_builder.default_timezone(v);
    }
    if let Some(v) = parts.join {
        fd_builder = fd_builder.join(v);
    }

    Ok(fd_builder.build())
}

/// Apply the six numeric-range constraint bounds (`min_rows`/`max_rows`,
/// `min_length`/`max_length`, `min`/`max`) parsed from the field table.
fn apply_constraint_bounds(
    mut builder: crate::core::FieldDefinitionBuilder,
    constraints: &super::constraints::Constraints,
) -> crate::core::FieldDefinitionBuilder {
    if let Some(v) = constraints.min_rows {
        builder = builder.min_rows(v);
    }
    if let Some(v) = constraints.max_rows {
        builder = builder.max_rows(v);
    }
    if let Some(v) = constraints.min_length {
        builder = builder.min_length(v);
    }
    if let Some(v) = constraints.max_length {
        builder = builder.max_length(v);
    }
    if let Some(v) = constraints.min {
        builder = builder.min(v);
    }
    if let Some(v) = constraints.max {
        builder = builder.max(v);
    }
    builder
}

/// Apply the optional `min_date`/`max_date` string bounds.
fn apply_date_bounds(
    mut builder: crate::core::FieldDefinitionBuilder,
    field_tbl: &Table,
) -> crate::core::FieldDefinitionBuilder {
    if let Some(v) = get_string(field_tbl, "min_date") {
        builder = builder.min_date(v);
    }
    if let Some(v) = get_string(field_tbl, "max_date") {
        builder = builder.max_date(v);
    }
    builder
}

pub(in crate::hooks::lua_api::parse) fn parse_field_access(access_tbl: &Table) -> FieldAccess {
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
    use serde_json::Value as JsonValue;

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
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
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
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
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
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
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
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
        assert_eq!(fields[0].default_value, Some(JsonValue::Number(42.into())));
    }

    #[test]
    fn test_parse_fields_default_value_float() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "ratio").unwrap();
        field.set("type", "number").unwrap();
        field.set("default_value", 3.15f64).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
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
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
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
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
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
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
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
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
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
        field.set("picker_appearance", "dayAndTime").unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
        assert_eq!(
            fields[0].picker_appearance,
            Some(crate::core::PickerAppearance::DayAndTime)
        );
    }

    #[test]
    fn test_parse_fields_date_picker_appearance_invalid_is_dropped() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "published_at").unwrap();
        field.set("type", "date").unwrap();
        field.set("picker_appearance", "datetime").unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
        // Unknown value gets warned + dropped — see `parse_date_config`.
        assert!(fields[0].picker_appearance.is_none());
    }

    #[test]
    fn test_parse_fields_non_date_picker_appearance_ignored() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "title").unwrap();
        field.set("type", "text").unwrap();
        field.set("picker_appearance", "dayAndTime").unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
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
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
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
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
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
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
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
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
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
        let result = parse_fields(&lua, &fields_tbl);
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
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
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
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
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
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
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
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
        let hooks = &fields[0].hooks;
        assert_eq!(hooks.before_validate, vec!["hooks.validate_title"]);
        assert_eq!(hooks.before_change, vec!["hooks.transform_title"]);
        assert_eq!(hooks.after_change, vec!["hooks.after_title_change"]);
        assert_eq!(hooks.after_read, vec!["hooks.format_title"]);
    }

    #[test]
    fn test_parse_fields_min_exceeds_max_rejected() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "score").unwrap();
        field.set("type", "number").unwrap();
        field.set("min", 100.0f64).unwrap();
        field.set("max", 10.0f64).unwrap();
        fields_tbl.set(1, field).unwrap();
        let err = parse_fields(&lua, &fields_tbl).unwrap_err();
        assert!(err.to_string().contains("min"), "{}", err);
    }

    #[test]
    fn test_parse_fields_min_length_exceeds_max_length_rejected() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "slug").unwrap();
        field.set("type", "text").unwrap();
        field.set("min_length", 100usize).unwrap();
        field.set("max_length", 10usize).unwrap();
        fields_tbl.set(1, field).unwrap();
        let err = parse_fields(&lua, &fields_tbl).unwrap_err();
        assert!(err.to_string().contains("min_length"), "{}", err);
    }

    #[test]
    fn test_parse_fields_min_rows_exceeds_max_rows_rejected() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "items").unwrap();
        field.set("type", "array").unwrap();
        field.set("min_rows", 10usize).unwrap();
        field.set("max_rows", 3usize).unwrap();
        fields_tbl.set(1, field).unwrap();
        let err = parse_fields(&lua, &fields_tbl).unwrap_err();
        assert!(err.to_string().contains("min_rows"), "{}", err);
    }

    #[test]
    fn test_parse_fields_date_timezone_enabled() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "event_at").unwrap();
        field.set("type", "date").unwrap();
        field.set("picker_appearance", "dayAndTime").unwrap();
        field.set("timezone", true).unwrap();
        field.set("default_timezone", "America/New_York").unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
        assert!(fields[0].timezone, "timezone should be true");
        assert_eq!(
            fields[0].default_timezone.as_deref(),
            Some("America/New_York")
        );
    }

    #[test]
    fn test_parse_fields_date_timezone_default_false() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "published_at").unwrap();
        field.set("type", "date").unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
        assert!(!fields[0].timezone, "timezone should default to false");
        assert!(fields[0].default_timezone.is_none());
    }

    #[test]
    fn test_parse_fields_timezone_ignored_for_day_only() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "birthday").unwrap();
        field.set("type", "date").unwrap();
        field.set("picker_appearance", "dayOnly").unwrap();
        field.set("timezone", true).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
        assert!(
            !fields[0].timezone,
            "timezone should be ignored for dayOnly"
        );
    }

    #[test]
    fn test_parse_fields_timezone_ignored_for_default_appearance() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "birthday").unwrap();
        field.set("type", "date").unwrap();
        // No picker_appearance set — defaults to dayOnly
        field.set("timezone", true).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
        assert!(
            !fields[0].timezone,
            "timezone should be ignored when picker_appearance defaults to dayOnly"
        );
    }

    #[test]
    fn test_parse_fields_timezone_ignored_for_time_only() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "alarm").unwrap();
        field.set("type", "date").unwrap();
        field.set("picker_appearance", "timeOnly").unwrap();
        field.set("timezone", true).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
        assert!(
            !fields[0].timezone,
            "timezone should be ignored for timeOnly"
        );
    }

    #[test]
    fn test_parse_fields_timezone_ignored_for_non_date() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "title").unwrap();
        field.set("type", "text").unwrap();
        field.set("timezone", true).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
        assert!(
            !fields[0].timezone,
            "timezone should be ignored for non-date fields"
        );
    }

    /// Regression: boolean default on a text field must be rejected.
    #[test]
    fn test_parse_fields_default_value_type_mismatch_text_boolean() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "title").unwrap();
        field.set("type", "text").unwrap();
        field.set("default_value", true).unwrap();
        fields_tbl.set(1, field).unwrap();
        let err = parse_fields(&lua, &fields_tbl).unwrap_err();
        assert!(
            err.to_string().contains("default_value type mismatch"),
            "Expected type mismatch error: {err}",
        );
    }

    /// Regression: string default on a number field must be rejected.
    #[test]
    fn test_parse_fields_default_value_type_mismatch_number_string() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "count").unwrap();
        field.set("type", "number").unwrap();
        field.set("default_value", "not-a-number").unwrap();
        fields_tbl.set(1, field).unwrap();
        let err = parse_fields(&lua, &fields_tbl).unwrap_err();
        assert!(
            err.to_string().contains("default_value type mismatch"),
            "Expected type mismatch error: {err}",
        );
    }

    /// Regression: number default on a checkbox must be rejected.
    #[test]
    fn test_parse_fields_default_value_type_mismatch_checkbox_number() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "active").unwrap();
        field.set("type", "checkbox").unwrap();
        field.set("default_value", 42i64).unwrap();
        fields_tbl.set(1, field).unwrap();
        let err = parse_fields(&lua, &fields_tbl).unwrap_err();
        assert!(
            err.to_string().contains("default_value type mismatch"),
            "Expected type mismatch error: {err}",
        );
    }

    /// Correct type combinations should still pass.
    #[test]
    fn test_parse_fields_default_value_correct_types_pass() {
        let lua = Lua::new();

        // Boolean default on checkbox — OK
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "active").unwrap();
        field.set("type", "checkbox").unwrap();
        field.set("default_value", true).unwrap();
        fields_tbl.set(1, field).unwrap();
        assert!(parse_fields(&lua, &fields_tbl).is_ok());

        // String default on text — OK
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "title").unwrap();
        field.set("type", "text").unwrap();
        field.set("default_value", "hello").unwrap();
        fields_tbl.set(1, field).unwrap();
        assert!(parse_fields(&lua, &fields_tbl).is_ok());

        // Number default on number — OK
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "count").unwrap();
        field.set("type", "number").unwrap();
        field.set("default_value", 10i64).unwrap();
        fields_tbl.set(1, field).unwrap();
        assert!(parse_fields(&lua, &fields_tbl).is_ok());
    }

    /// BUG-2 regression: two sibling fields with the same name must fail
    /// at parse time instead of silently overwriting each other at runtime.
    #[test]
    fn parse_fields_rejects_duplicate_name_at_same_level() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();

        let f1 = lua.create_table().unwrap();
        f1.set("name", "title").unwrap();
        f1.set("type", "text").unwrap();
        fields_tbl.set(1, f1).unwrap();

        let f2 = lua.create_table().unwrap();
        f2.set("name", "title").unwrap();
        f2.set("type", "text").unwrap();
        fields_tbl.set(2, f2).unwrap();

        let err = parse_fields(&lua, &fields_tbl).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("title") && msg.contains("Duplicate"),
            "expected duplicate-name error, got: {msg}"
        );
    }

    /// BUG-2 regression: layout wrappers (Row) are transparent, so a name
    /// repeated between a top-level field and a Row child counts as a duplicate.
    #[test]
    fn parse_fields_rejects_duplicate_across_layout_wrappers() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();

        // Top-level: title
        let f1 = lua.create_table().unwrap();
        f1.set("name", "title").unwrap();
        f1.set("type", "text").unwrap();
        fields_tbl.set(1, f1).unwrap();

        // Row wrapping another "title" — also transparent → duplicate.
        let row = lua.create_table().unwrap();
        row.set("name", "row1").unwrap();
        row.set("type", "row").unwrap();
        let row_fields = lua.create_table().unwrap();
        let inner = lua.create_table().unwrap();
        inner.set("name", "title").unwrap();
        inner.set("type", "text").unwrap();
        row_fields.set(1, inner).unwrap();
        row.set("fields", row_fields).unwrap();
        fields_tbl.set(2, row).unwrap();

        let err = parse_fields(&lua, &fields_tbl).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("title"),
            "expected duplicate-name error naming the field, got: {msg}"
        );
    }

    /// BUG-2 regression: Group fields create their own namespace
    /// (columns are prefixed `group__name`), so the same sub-field name
    /// can appear in two different groups.
    #[test]
    fn parse_fields_allows_same_name_in_different_groups() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();

        let mk_group = |slot: i64, name: &str| {
            let g = lua.create_table().unwrap();
            g.set("name", name).unwrap();
            g.set("type", "group").unwrap();
            let sub = lua.create_table().unwrap();
            let s = lua.create_table().unwrap();
            s.set("name", "label").unwrap();
            s.set("type", "text").unwrap();
            sub.set(1, s).unwrap();
            g.set("fields", sub).unwrap();
            fields_tbl.set(slot, g).unwrap();
        };

        mk_group(1, "hero");
        mk_group(2, "footer");

        // Should NOT error — `hero.label` and `footer.label` are distinct columns.
        parse_fields(&lua, &fields_tbl).expect("groups namespace sub-fields independently");
    }

    /// Helper: parse a single-field collection with the given name/type.
    fn parse_one(lua: &Lua, name: &str, ty: &str) -> Result<Vec<FieldDefinition>> {
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", name).unwrap();
        field.set("type", ty).unwrap();
        fields_tbl.set(1, field).unwrap();
        parse_fields(lua, &fields_tbl)
    }

    /// Regression: a field named after a non-`_` system column (`id`,
    /// `parent_id`, `created_at`, `updated_at`) must be rejected at parse
    /// time — it would otherwise collide with an auto-generated column.
    #[test]
    fn parse_rejects_reserved_system_column_names() {
        let lua = Lua::new();
        for name in ["id", "parent_id", "created_at", "updated_at"] {
            let err = parse_one(&lua, name, "text").unwrap_err().to_string();
            assert!(
                err.contains("reserved") && err.contains(name),
                "expected reserved-name rejection for '{name}', got: {err}"
            );
        }
    }

    /// Regression: a field whose name starts with `_` must be rejected — the
    /// underscore prefix is reserved for system columns.
    #[test]
    fn parse_rejects_underscore_prefixed_field_names() {
        let lua = Lua::new();
        for name in ["_status", "_ref_count", "_order", "_internal"] {
            let err = parse_one(&lua, name, "text").unwrap_err().to_string();
            assert!(
                err.contains("underscore"),
                "expected underscore rejection for '{name}', got: {err}"
            );
        }
    }

    /// A field whose name merely *contains* (but doesn't start with) an
    /// underscore, or shares a prefix with a reserved name, is still valid.
    #[test]
    fn parse_allows_non_reserved_names_near_system_columns() {
        let lua = Lua::new();
        for name in ["identifier", "parent", "created", "order", "status"] {
            parse_one(&lua, name, "text")
                .unwrap_or_else(|e| panic!("'{name}' should be a valid field name, got: {e}"));
        }
    }
}
