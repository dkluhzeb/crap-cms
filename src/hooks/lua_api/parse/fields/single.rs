//! Per-field parsing: orchestrates the full Lua → `FieldDefinition` conversion
//! for a single field table. Phase 1 (here) extracts every sub-structure;
//! phase 2 ([`assemble`]) folds them into the definition. The accepted keys
//! live in [`keys`], the optional config tables in [`configs`], and the
//! `access` / `hooks` sub-tables in [`access_hooks`].

mod access_hooks;
mod assemble;
mod configs;
mod keys;

#[cfg(test)]
mod tests;

use anyhow::{Result, anyhow, bail};
use mlua::{Lua, Table};

use crate::{
    core::{
        BlockDefinition, FieldAccess, FieldAdmin, FieldDefinition, FieldHooks, FieldTab, FieldType,
        JoinConfig, LANG_SUFFIX, McpFieldConfig, PickerAppearance, RelationshipConfig,
        SelectOption, TZ_SUFFIX, any_field, is_reserved_field_name,
    },
    db::query,
    hooks::lua_api::parse::{
        admin::{deny_type_scoped_admin_keys, parse_field_admin},
        blocks::{parse_block_definitions, parse_tab_definitions},
        helpers::{deny_unknown_keys, get_string_val, get_table, parse_select_options},
        relationship::parse_field_relationship,
    },
};

use super::{
    constraints::{Constraints, parse_constraints, parse_date_config, parse_default_value},
    top::parse_fields,
};

use self::{
    access_hooks::{parse_field_access, parse_field_hooks},
    assemble::assemble_field_definition,
    configs::{parse_join, parse_mcp},
    keys::validate_field_keys,
};

pub(crate) use self::{access_hooks::FIELD_HOOK_KEYS, configs::parse_required_locales};

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

    // The non-`_`-prefixed auto columns (the main-table primary key, the
    // timestamps, the `parent_id` FK on array/blocks join tables) plus the
    // `collection` tag every populated relationship target carries. One list
    // shared with `is_system_column` via `core::AUTO_COLUMNS`, so they can't drift.
    if is_reserved_field_name(&name) {
        bail!(
            "Field name '{name}' is reserved — it collides with an automatically generated \
             column (id, parent_id, created_at, updated_at) or the 'collection' tag of a \
             populated relationship target"
        );
    }

    // The `_tz` / `_lang` suffixes are reserved for the companion columns
    // synthesized for timezone-aware Date fields and language-picker Code
    // fields (`{field}_tz`, `{field}_lang`). A user field literally named
    // `start_date_tz` would collide with the companion of `start_date`.
    // Reserving the suffix pattern is the safe freeze direction — it can be
    // loosened later (additive) but never added post-freeze.
    if name.ends_with(TZ_SUFFIX) || name.ends_with(LANG_SUFFIX) {
        bail!(
            "Field name '{name}' is reserved — the '_tz' and '_lang' suffixes are used for \
             timezone/language companion columns (e.g. a Date field's '{{field}}_tz')"
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
    relationship: Option<RelationshipConfig>,
    picker_appearance: Option<PickerAppearance>,
    timezone: bool,
    default_timezone: Option<String>,
    constraints: Constraints,
    options: Vec<SelectOption>,
    admin: FieldAdmin,
    hooks: FieldHooks,
    access: FieldAccess,
    sub_fields: Vec<FieldDefinition>,
    block_defs: Vec<BlockDefinition>,
    tab_defs: Vec<FieldTab>,
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

    // An absent `type` defaults to text; a PRESENT-but-unknown type is a hard
    // error (never silently coerced to Text — that would freeze the wrong
    // column shape and lock out ever adding a real field type of that name).
    let field_type = match field_tbl.get::<Option<String>>("type")? {
        None => FieldType::Text,
        Some(type_str) => FieldType::parse(&type_str).ok_or_else(|| {
            anyhow!(
                "Field '{name}': unknown field type '{type_str}'. Valid types: {}",
                FieldType::ALL.join(", ")
            )
        })?,
    };

    validate_field_keys(field_tbl, &field_type)?;

    let default_value = parse_default_value(field_tbl, &name, &field_type)?;
    let relationship = parse_field_relationship(field_tbl, &field_type)?;
    let (picker_appearance, timezone, default_timezone) =
        parse_date_config(field_tbl, &name, &field_type)?;
    let constraints = parse_constraints(field_tbl, &name)?;

    let options =
        get_table(field_tbl, "options").map_or(Ok(Vec::new()), |tbl| parse_select_options(&tbl))?;

    let admin = get_table(field_tbl, "admin").map_or(Ok(FieldAdmin::default()), |tbl| {
        deny_type_scoped_admin_keys(&tbl, &field_type)?;
        parse_field_admin(&tbl)
    })?;

    let hooks = get_table(field_tbl, "hooks")
        .map_or(Ok(FieldHooks::default()), |tbl| parse_field_hooks(&tbl))?;

    // Transparent layout wrappers (row/collapsible/tabs) have no value of their
    // own, so a field lifecycle hook on them could never fire. Reject it loudly
    // at parse time rather than silently ignoring it — the hook belongs on a
    // child field. Group/Array/Blocks DO carry a value and run their own hook.
    if matches!(
        field_type,
        FieldType::Row | FieldType::Collapsible | FieldType::Tabs
    ) && !hooks.is_empty()
    {
        bail!(
            "{} field '{name}': lifecycle hooks are not supported on transparent \
             layout wrappers (row/collapsible/tabs) — they have no value of their \
             own; put the hook on a child field instead",
            field_type.as_str()
        );
    }

    let access = match get_table(field_tbl, "access") {
        Ok(tbl) => {
            deny_unknown_keys(&tbl, "field access", &["read", "create", "update"])?;
            parse_field_access(&tbl)?
        }
        Err(_) => FieldAccess::default(),
    };

    let sub_fields = parse_sub_fields(lua, field_tbl, &field_type)?;
    let block_defs = parse_block_defs(lua, field_tbl, &field_type)?;
    let tab_defs = parse_tab_defs(lua, field_tbl, &field_type)?;
    deny_nested_position(&name, &sub_fields, &block_defs, &tab_defs)?;
    deny_join_in_rows(&name, &field_type, &sub_fields, &block_defs)?;

    let join = parse_join(field_tbl, &field_type, &name)?;
    let mcp = parse_mcp(field_tbl)?;

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

/// `admin.position` places a field in the edit form's main column or its
/// sidebar — a choice only a top-level field makes: a field inside a group,
/// row, collapsible, tabs, array or blocks field renders with its container.
/// On a nested field the setting would be silently inert, so it is refused.
/// Each level checks its direct children, so every depth is covered.
fn deny_nested_position(
    name: &str,
    sub_fields: &[FieldDefinition],
    block_defs: &[BlockDefinition],
    tab_defs: &[FieldTab],
) -> Result<()> {
    let mut children = sub_fields
        .iter()
        .chain(block_defs.iter().flat_map(|block| &block.fields))
        .chain(tab_defs.iter().flat_map(|tab| &tab.fields));

    let Some(child) = children.find(|child| child.admin.position.is_some()) else {
        return Ok(());
    };

    bail!(
        "Field '{name}': sub-field '{}' sets admin.position, which applies only to \
         top-level fields — a nested field renders inside its container",
        child.name
    )
}

/// A join lists the documents whose `on` field references the document it
/// belongs to, so inside an array or blocks row it has no per-row meaning —
/// every row would list the same documents. Refused anywhere under a row
/// (inside a group or layout wrapper in the row too); a join belongs at the top
/// level, in a layout wrapper, or in a group.
fn deny_join_in_rows(
    name: &str,
    field_type: &FieldType,
    sub_fields: &[FieldDefinition],
    block_defs: &[BlockDefinition],
) -> Result<()> {
    if !matches!(field_type, FieldType::Array | FieldType::Blocks) {
        return Ok(());
    }

    let is_join = |f: &FieldDefinition| f.field_type == FieldType::Join;
    let in_rows = any_field(sub_fields, &is_join)
        || block_defs
            .iter()
            .any(|block| any_field(&block.fields, &is_join));

    if !in_rows {
        return Ok(());
    }

    bail!(
        "{} field '{name}': a join field cannot sit inside its rows — a join lists the \
         documents that reference the whole document, so every row would repeat the same \
         list; move the join to the top level, a group, or a row/collapsible/tabs wrapper",
        field_type.as_str()
    )
}

/// Parse the `blocks` sub-table — only meaningful for `FieldType::Blocks`.
fn parse_block_defs(
    lua: &Lua,
    field_tbl: &Table,
    field_type: &FieldType,
) -> Result<Vec<BlockDefinition>> {
    if *field_type != FieldType::Blocks {
        return Ok(Vec::new());
    }
    get_table(field_tbl, "blocks").map_or(Ok(Vec::new()), |tbl| parse_block_definitions(lua, &tbl))
}

/// Parse the `tabs` sub-table — only meaningful for `FieldType::Tabs`.
fn parse_tab_defs(lua: &Lua, field_tbl: &Table, field_type: &FieldType) -> Result<Vec<FieldTab>> {
    if *field_type != FieldType::Tabs {
        return Ok(Vec::new());
    }
    get_table(field_tbl, "tabs").map_or(Ok(Vec::new()), |tbl| parse_tab_definitions(lua, &tbl))
}
