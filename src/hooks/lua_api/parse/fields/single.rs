//! Per-field parsing: orchestrates the full Lua → `FieldDefinition` conversion
//! for a single field table, plus the small helper parsers (name, sub-fields,
//! access, hooks).
//!
//! NOTE ON SIZE: this file exceeds the ~1000-line soft cap, but the code itself
//! is ~600 lines; the remainder is an extensive `#[cfg(test)]` suite. Those tests
//! are integration tests of the single public entry point [`parse_single_field`]
//! (each drives a full Lua field table through it), so they do not partition
//! along any code-cohesion boundary. A split was deliberately declined: extracting
//! sub-parsers would leave the integration tests behind, and partitioning the
//! tests by feature would divorce them from the entry point they exercise — both
//! *lower* cohesion. Keeping the parser and its tests together is the cleaner
//! choice here (contrast `access::field`, which had genuinely separable
//! walk/check/strip concerns and was split).

use anyhow::{Result, anyhow, bail};
use chrono::NaiveDate;
use mlua::{Lua, Table, Value};

use crate::{
    core::{
        FieldAccess, FieldAdmin, FieldDefinition, FieldHooks, FieldType, JoinConfig,
        McpFieldConfig, RequiredLocales,
    },
    db::query::{
        self,
        helpers::{LANG_SUFFIX, TZ_SUFFIX},
    },
};

use super::super::admin::parse_field_admin;
use super::super::blocks::{parse_block_definitions, parse_tab_definitions};
use super::super::helpers::{
    deny_unknown_keys, get_bool, get_optional_hook_ref, get_string, get_string_val, get_table,
    parse_hook_ref_list, parse_select_options,
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

/// Keys accepted on every field table regardless of type. Type-specific keys
/// are appended by [`type_specific_field_keys`]. Unknown keys are rejected
/// (parity with the strict jobs config), so a typo like `requird` or a
/// misplaced key like `options` on a text field fails loudly at load time.
const COMMON_FIELD_KEYS: &[&str] = &[
    "name",
    "type",
    "required",
    "required_when",
    "unique",
    "index",
    "localized",
    "required_locales",
    "hidden",
    "validate",
    "default_value",
    "admin",
    "hooks",
    "access",
    "mcp",
];

/// Keys valid only on specific field types, appended to [`COMMON_FIELD_KEYS`].
/// `min_rows`/`max_rows` bound the count of multi-value fields; the string
/// length bounds apply to text-backed types; `relation_to` is the legacy flat
/// relationship syntax kept for back-compat.
fn type_specific_field_keys(field_type: &FieldType) -> &'static [&'static str] {
    match field_type {
        FieldType::Text => &[
            "has_many",
            "min_length",
            "max_length",
            "min_rows",
            "max_rows",
        ],
        FieldType::Number => &["has_many", "min", "max", "integer", "min_rows", "max_rows"],
        FieldType::Textarea | FieldType::Richtext | FieldType::Email | FieldType::Code => {
            &["min_length", "max_length"]
        }
        FieldType::Select | FieldType::Radio => &["options", "has_many", "min_rows", "max_rows"],
        FieldType::Checkbox | FieldType::Json => &[],
        FieldType::Date => &[
            "min_date",
            "max_date",
            "picker_appearance",
            "timezone",
            "default_timezone",
        ],
        FieldType::Relationship | FieldType::Upload => &[
            "relationship",
            "relation_to",
            "has_many",
            "min_rows",
            "max_rows",
        ],
        FieldType::Array => &["fields", "min_rows", "max_rows"],
        FieldType::Group | FieldType::Row | FieldType::Collapsible => &["fields"],
        FieldType::Tabs => &["tabs"],
        FieldType::Blocks => &["blocks", "min_rows", "max_rows"],
        FieldType::Join => &["collection", "on"],
    }
}

/// Reject any key on a field table that is not valid for its type.
fn validate_field_keys(field_tbl: &Table, field_type: &FieldType) -> Result<()> {
    let mut allowed: Vec<&str> = COMMON_FIELD_KEYS.to_vec();
    allowed.extend_from_slice(type_specific_field_keys(field_type));

    deny_unknown_keys(
        field_tbl,
        &format!("{} field", field_type.as_str()),
        &allowed,
    )
}

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

    let admin = get_table(field_tbl, "admin")
        .map_or(Ok(FieldAdmin::default()), |tbl| parse_field_admin(&tbl))?;

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

/// Build the `JoinConfig` for `FieldType::Join`. `collection` and `on` are
/// required non-empty strings — a join without them can never resolve, so a
/// missing, wrong-typed, or empty value is a load-time error instead of a
/// silently-empty config that matches nothing at populate time.
fn parse_join(field_tbl: &Table, field_type: &FieldType, name: &str) -> Result<Option<JoinConfig>> {
    if *field_type != FieldType::Join {
        return Ok(None);
    }

    let required = |key: &str| -> Result<String> {
        match field_tbl.get::<Value>(key)? {
            Value::String(s) => {
                let s = s.to_str()?.to_string();
                if s.is_empty() {
                    bail!("join field '{name}': '{key}' must be a non-empty string");
                }
                Ok(s)
            }
            Value::Nil => bail!("join field '{name}': missing required key '{key}'"),
            other => bail!(
                "join field '{name}': '{key}' must be a string, got {}",
                other.type_name()
            ),
        }
    };

    let collection = required("collection")?;
    let on = required("on")?;

    Ok(Some(JoinConfig::new(collection, on)))
}

/// Parse the optional `mcp` sub-table for MCP introspection metadata.
fn parse_mcp(field_tbl: &Table) -> Result<McpFieldConfig> {
    let Ok(tbl) = get_table(field_tbl, "mcp") else {
        return Ok(McpFieldConfig::default());
    };

    deny_unknown_keys(&tbl, "field mcp", &["description"])?;

    Ok(McpFieldConfig {
        description: get_string(&tbl, "description"),
    })
}

/// Parse a `required_locales` value from a config table: the string `"all"`
/// or a non-empty list of locale codes. `None` when the key is absent.
pub(crate) fn parse_required_locales(tbl: &Table) -> Result<Option<RequiredLocales>> {
    let val: mlua::Value = tbl.get("required_locales")?;
    match val {
        mlua::Value::Nil => Ok(None),
        mlua::Value::String(s) => {
            let s = s.to_str()?;
            if &*s == "all" {
                Ok(Some(RequiredLocales::All))
            } else {
                bail!("required_locales string must be \"all\", got '{}'", &*s)
            }
        }
        mlua::Value::Table(t) => {
            let locales = t
                .sequence_values::<String>()
                .collect::<mlua::Result<Vec<_>>>()?;
            if locales.is_empty() {
                bail!(
                    "required_locales list must not be empty (omit the key for default behavior)"
                );
            }
            Ok(Some(RequiredLocales::List(locales)))
        }
        other => bail!(
            "required_locales must be \"all\" or a list of locale codes, got {}",
            other.type_name()
        ),
    }
}

/// Phase 2 — fold parsed parts into a [`FieldDefinition`] via the builder.
/// A `Join` is a virtual, read-only reverse-relationship with no stored value,
/// so `required` / `localized` / `required_locales` are meaningless on it.
/// Reject them at load instead of silently ignoring them (the previous
/// behavior, which also let a `localized + required` Join wedge non-draft writes
/// — the validation walkers now skip Join as defense-in-depth, but the config is
/// still nonsensical).
fn reject_meaningless_join_flags(
    field_tbl: &Table,
    field_type: &FieldType,
    name: &str,
) -> Result<()> {
    if *field_type != FieldType::Join {
        return Ok(());
    }

    for key in ["required", "localized"] {
        if get_bool(field_tbl, key, false)? {
            bail!(
                "join field '{name}': '{key}' is meaningless — a Join is a virtual, \
                 read-only reverse-relationship with no stored value"
            );
        }
    }
    if parse_required_locales(field_tbl)?.is_some() {
        bail!("join field '{name}': 'required_locales' is meaningless on a virtual Join field");
    }

    Ok(())
}

fn assemble_field_definition(
    field_tbl: &Table,
    parts: ParsedFieldParts,
) -> Result<FieldDefinition> {
    reject_meaningless_join_flags(field_tbl, &parts.field_type, &parts.name)?;

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

    if let Some(v) = parse_required_locales(field_tbl)? {
        // Whether `required_locales` is meaningful (the field is locale-scoped —
        // possibly by inheriting localization from an enclosing group) is
        // validated at startup in `startup_checks::validate_required_locales`,
        // where the full field tree (and thus group inheritance) is known. The
        // per-field parser only sees this field's own `localized` flag.
        fd_builder = fd_builder.required_locales(v);
    }

    if let Some(v) = get_optional_hook_ref(field_tbl, "validate", "field validate")? {
        fd_builder = fd_builder.validate(v);
    }
    if let Some(v) = get_optional_hook_ref(field_tbl, "required_when", "required_when")? {
        fd_builder = fd_builder.required_when(v);
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
    fd_builder = apply_date_bounds(fd_builder, field_tbl, &parts.name)?;

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
    if constraints.integer {
        builder = builder.integer(true);
    }
    builder
}

/// Apply the optional `min_date`/`max_date` bounds. Each bound must be a
/// `YYYY-MM-DD` string: the runtime check compares the submitted value's date
/// part lexically against the bound, so a wrong type or format would make the
/// bound silently never (or always) match. `min_date` after `max_date` is
/// rejected outright.
fn apply_date_bounds(
    mut builder: crate::core::FieldDefinitionBuilder,
    field_tbl: &Table,
    name: &str,
) -> Result<crate::core::FieldDefinitionBuilder> {
    let min = get_date_bound(field_tbl, "min_date", name)?;
    let max = get_date_bound(field_tbl, "max_date", name)?;

    if let (Some(min), Some(max)) = (&min, &max)
        && min > max
    {
        bail!("date field '{name}': min_date '{min}' is after max_date '{max}'");
    }

    if let Some(v) = min {
        builder = builder.min_date(v);
    }
    if let Some(v) = max {
        builder = builder.max_date(v);
    }

    Ok(builder)
}

/// Read one strict `YYYY-MM-DD` date bound.
fn get_date_bound(field_tbl: &Table, key: &str, name: &str) -> Result<Option<String>> {
    match field_tbl.get::<Value>(key)? {
        Value::Nil => Ok(None),
        Value::String(s) => {
            let s = s.to_str()?.to_string();
            if NaiveDate::parse_from_str(&s, "%Y-%m-%d").is_err() {
                bail!("date field '{name}': {key} '{s}' is not a YYYY-MM-DD date");
            }
            Ok(Some(s))
        }
        other => bail!(
            "date field '{name}': {key} must be a YYYY-MM-DD string, got {}",
            other.type_name()
        ),
    }
}

/// Parse a field's `access` sub-table. Each rule is a string hook reference;
/// a present-but-non-string value is a hard error rather than being silently
/// dropped (parity with collection/global `access`).
///
/// # Errors
///
/// Returns an error if `read`/`create`/`update` is present but not a string.
pub(in crate::hooks::lua_api::parse) fn parse_field_access(
    access_tbl: &Table,
) -> Result<FieldAccess> {
    Ok(FieldAccess {
        read: get_optional_hook_ref(access_tbl, "read", "field access")?,
        create: get_optional_hook_ref(access_tbl, "create", "field access")?,
        update: get_optional_hook_ref(access_tbl, "update", "field access")?,
    })
}

fn parse_field_hooks(hooks_tbl: &Table) -> Result<FieldHooks> {
    deny_unknown_keys(
        hooks_tbl,
        "field hooks",
        &[
            "before_validate",
            "before_change",
            "after_change",
            "after_read",
        ],
    )?;

    Ok(FieldHooks {
        before_validate: parse_hook_ref_list(hooks_tbl, "before_validate")?,
        before_change: parse_hook_ref_list(hooks_tbl, "before_change")?,
        after_change: parse_hook_ref_list(hooks_tbl, "after_change")?,
        after_read: parse_hook_ref_list(hooks_tbl, "after_read")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::HookRef;
    use mlua::Lua;
    use serde_json::Value as JsonValue;

    #[test]
    fn test_parse_field_access() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("read", "hooks.access.check_role").unwrap();
        tbl.set("create", "hooks.access.admin_only").unwrap();
        let access = parse_field_access(&tbl).unwrap();
        assert_eq!(
            access.read.as_ref().map(HookRef::reference),
            Some("hooks.access.check_role")
        );
        assert_eq!(
            access.create.as_ref().map(HookRef::reference),
            Some("hooks.access.admin_only")
        );
        assert!(access.update.is_none());
    }

    /// Regression: a non-string field access rule is a hard error, not
    /// silently dropped (parity with collection/global `access`).
    #[test]
    fn test_parse_field_access_non_string_errors() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("update", 1i64).unwrap();
        let err = parse_field_access(&tbl).unwrap_err().to_string();
        assert!(err.contains("field access"), "got: {err}");
        assert!(err.contains("update"), "got: {err}");
    }

    fn field_with(lua: &Lua, set: impl FnOnce(&Table)) -> Result<()> {
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "f").unwrap();
        set(&field);
        fields_tbl.set(1, field).unwrap();
        parse_fields(lua, &fields_tbl).map(|_| ())
    }

    #[test]
    fn test_unknown_field_key_is_rejected() {
        let lua = Lua::new();
        // `requird` is a typo for `required`.
        let err = field_with(&lua, |f| {
            f.set("type", "text").unwrap();
            f.set("requird", true).unwrap();
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("requird"), "{err}");
        assert!(
            err.contains("required"),
            "should suggest closest key: {err}"
        );
    }

    #[test]
    fn test_misplaced_key_rejected_per_type() {
        let lua = Lua::new();
        // `options` is valid on select, not on text — per-type check must reject it.
        let err = field_with(&lua, |f| {
            f.set("type", "text").unwrap();
            let opts = lua.create_table().unwrap();
            f.set("options", opts).unwrap();
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("options"), "{err}");
    }

    #[test]
    fn test_type_specific_key_accepted_on_right_type() {
        let lua = Lua::new();
        // `options` on a select field is valid.
        assert!(
            field_with(&lua, |f| {
                f.set("type", "select").unwrap();
                let opts = lua.create_table().unwrap();
                f.set("options", opts).unwrap();
            })
            .is_ok()
        );
        // Legacy `relation_to` on a relationship field stays valid.
        assert!(
            field_with(&lua, |f| {
                f.set("type", "relationship").unwrap();
                f.set("relation_to", "users").unwrap();
            })
            .is_ok()
        );
    }

    /// A row/collapsible/tabs child for a hook-placement test.
    fn set_layout_with_hook(lua: &Lua, f: &Table, ty: &str) {
        f.set("type", ty).unwrap();

        let fields = lua.create_table().unwrap();
        let child = lua.create_table().unwrap();
        child.set("name", "title").unwrap();
        child.set("type", "text").unwrap();
        fields.set(1, child).unwrap();

        if ty == "tabs" {
            // Tabs nest fields under a tab, not directly.
            let tab = lua.create_table().unwrap();
            tab.set("label", "Tab").unwrap();
            tab.set("fields", fields).unwrap();
            let tabs = lua.create_table().unwrap();
            tabs.set(1, tab).unwrap();
            f.set("tabs", tabs).unwrap();
        } else {
            f.set("fields", fields).unwrap();
        }

        let hooks = lua.create_table().unwrap();
        let bc = lua.create_table().unwrap();
        bc.set(1, "hooks.transform").unwrap();
        hooks.set("before_change", bc).unwrap();
        f.set("hooks", hooks).unwrap();
    }

    /// Regression: a lifecycle hook placed on a transparent layout wrapper
    /// (row/collapsible/tabs) is rejected at parse time. These wrappers carry no
    /// value, so the hook could never fire — it must fail loudly, not be a
    /// silent no-op.
    #[test]
    fn test_lifecycle_hook_on_layout_wrapper_rejected() {
        for ty in ["row", "collapsible", "tabs"] {
            let lua = Lua::new();
            let err = field_with(&lua, |f| set_layout_with_hook(&lua, f, ty))
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("layout wrapper") || err.contains("transparent"),
                "{ty}: {err}"
            );
        }
    }

    /// A lifecycle hook on a Group is valid — the group carries a value and runs
    /// its own hook (parity with array/blocks).
    #[test]
    fn test_lifecycle_hook_on_group_accepted() {
        let lua = Lua::new();
        assert!(
            field_with(&lua, |f| set_layout_with_hook(&lua, f, "group")).is_ok(),
            "a hook on a group field must be accepted"
        );
    }

    #[test]
    fn test_unknown_admin_key_is_rejected() {
        let lua = Lua::new();
        let err = field_with(&lua, |f| {
            f.set("type", "text").unwrap();
            let admin = lua.create_table().unwrap();
            admin.set("lable", "Title").unwrap(); // typo for `label`
            f.set("admin", admin).unwrap();
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("lable"), "{err}");
    }

    #[test]
    fn test_admin_extra_escape_hatch_allows_custom_keys() {
        let lua = Lua::new();
        // Arbitrary keys under admin.extra are the plugin escape hatch.
        assert!(
            field_with(&lua, |f| {
                f.set("type", "text").unwrap();
                let admin = lua.create_table().unwrap();
                let extra = lua.create_table().unwrap();
                extra.set("max_stars", 5).unwrap();
                admin.set("extra", extra).unwrap();
                f.set("admin", admin).unwrap();
            })
            .is_ok()
        );
    }

    #[test]
    fn test_unknown_field_nested_keys_rejected() {
        let lua = Lua::new();

        // field hooks
        let err = field_with(&lua, |f| {
            f.set("type", "text").unwrap();
            let hooks = lua.create_table().unwrap();
            hooks
                .set("before_save", lua.create_table().unwrap())
                .unwrap(); // not a real field hook
            f.set("hooks", hooks).unwrap();
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("before_save"), "{err}");

        // field access
        assert!(
            field_with(&lua, |f| {
                f.set("type", "text").unwrap();
                let access = lua.create_table().unwrap();
                access.set("delete", "hooks.x").unwrap(); // field access has no `delete`
                f.set("access", access).unwrap();
            })
            .is_err()
        );

        // field mcp
        assert!(
            field_with(&lua, |f| {
                f.set("type", "text").unwrap();
                let mcp = lua.create_table().unwrap();
                mcp.set("desc", "x").unwrap(); // not `description`
                f.set("mcp", mcp).unwrap();
            })
            .is_err()
        );
    }

    #[test]
    fn test_unknown_relationship_key_rejected() {
        let lua = Lua::new();
        let err = field_with(&lua, |f| {
            f.set("type", "relationship").unwrap();
            let rel = lua.create_table().unwrap();
            rel.set("collection", "users").unwrap();
            rel.set("depth", 2).unwrap(); // not `max_depth`
            f.set("relationship", rel).unwrap();
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("depth"), "{err}");
    }

    #[test]
    fn test_unknown_select_option_key_rejected() {
        let lua = Lua::new();
        let err = field_with(&lua, |f| {
            f.set("type", "select").unwrap();
            let opts = lua.create_table().unwrap();
            let opt = lua.create_table().unwrap();
            opt.set("label", "Draft").unwrap();
            opt.set("val", "draft").unwrap(); // not `value`
            opts.set(1, opt).unwrap();
            f.set("options", opts).unwrap();
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("val"), "{err}");
    }

    #[test]
    fn test_unknown_block_and_tab_keys_rejected() {
        let lua = Lua::new();

        // block definition
        let err = field_with(&lua, |f| {
            f.set("type", "blocks").unwrap();
            let blocks = lua.create_table().unwrap();
            let block = lua.create_table().unwrap();
            block.set("type", "hero").unwrap();
            block.set("labl", "Hero").unwrap(); // typo for `label`
            blocks.set(1, block).unwrap();
            f.set("blocks", blocks).unwrap();
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("labl"), "{err}");

        // tab definition
        assert!(
            field_with(&lua, |f| {
                f.set("type", "tabs").unwrap();
                let tabs = lua.create_table().unwrap();
                let tab = lua.create_table().unwrap();
                tab.set("label", "General").unwrap();
                tab.set("desc", "x").unwrap(); // not `description`
                tabs.set(1, tab).unwrap();
                f.set("tabs", tabs).unwrap();
            })
            .is_err()
        );
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
    fn test_parse_fields_non_date_picker_appearance_rejected() {
        // Strict per-type validation: `picker_appearance` is a date-only key,
        // so it is rejected (not silently ignored) on a text field.
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "title").unwrap();
        field.set("type", "text").unwrap();
        field.set("picker_appearance", "dayAndTime").unwrap();
        fields_tbl.set(1, field).unwrap();
        let err = parse_fields(&lua, &fields_tbl).unwrap_err().to_string();
        assert!(err.contains("picker_appearance"), "{err}");
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
        assert_eq!(
            hooks.before_validate,
            vec![HookRef::new("hooks.validate_title")]
        );
        assert_eq!(
            hooks.before_change,
            vec![HookRef::new("hooks.transform_title")]
        );
        assert_eq!(
            hooks.after_change,
            vec![HookRef::new("hooks.after_title_change")]
        );
        assert_eq!(hooks.after_read, vec![HookRef::new("hooks.format_title")]);
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
    fn test_parse_fields_timezone_rejected_for_non_date() {
        // Strict per-type validation: `timezone` is a date-only key, so it is
        // rejected (not silently ignored) on a text field.
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "title").unwrap();
        field.set("type", "text").unwrap();
        field.set("timezone", true).unwrap();
        fields_tbl.set(1, field).unwrap();
        let err = parse_fields(&lua, &fields_tbl).unwrap_err().to_string();
        assert!(err.contains("timezone"), "{err}");
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

    /// Regression: a present-but-unknown field `type` must be rejected, not
    /// silently coerced to `Text` (which would freeze the wrong column shape
    /// and lock out ever adding a real field type of that name).
    #[test]
    fn parse_rejects_unknown_field_type() {
        let lua = Lua::new();
        for ty in ["slug", "tex", "relation", "Text ", "richtext2"] {
            let err = parse_one(&lua, "f", ty).unwrap_err().to_string();
            assert!(
                err.contains("unknown field type") && err.contains(ty.trim()),
                "expected unknown-type rejection for '{ty}', got: {err}"
            );
        }
    }

    /// An omitted `type` still defaults to text (absent ≠ unknown).
    #[test]
    fn parse_defaults_absent_type_to_text() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "f").unwrap();
        fields_tbl.set(1, field).unwrap();
        let parsed = parse_fields(&lua, &fields_tbl).expect("absent type defaults to text");
        assert_eq!(parsed[0].field_type, FieldType::Text);
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

    /// Regression: `_tz` / `_lang` suffixes are reserved for the timezone /
    /// language companion columns — a field named `start_date_tz` would
    /// collide with the companion of `start_date`.
    #[test]
    fn parse_rejects_tz_and_lang_suffix_field_names() {
        let lua = Lua::new();
        for name in ["start_date_tz", "snippet_lang", "event_tz", "content_lang"] {
            let err = parse_one(&lua, name, "text").unwrap_err().to_string();
            assert!(
                err.contains("reserved") && (err.contains("_tz") || err.contains("_lang")),
                "expected suffix rejection for '{name}', got: {err}"
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

    // ── join strictness ─────────────────────────────────────────────────

    /// Regression: a join field with missing / wrong-typed / empty
    /// `collection`/`on` used to silently become empty strings ("validated
    /// later" — which never happened) and matched nothing at populate time.
    /// All three are now load-time errors.
    #[test]
    fn test_join_missing_collection_rejected() {
        let lua = Lua::new();
        let err = field_with(&lua, |f| {
            f.set("type", "join").unwrap();
            f.set("on", "author").unwrap();
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("collection"), "{err}");
    }

    #[test]
    fn test_join_non_string_on_rejected() {
        let lua = Lua::new();
        let err = field_with(&lua, |f| {
            f.set("type", "join").unwrap();
            f.set("collection", "posts").unwrap();
            f.set("on", true).unwrap();
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("'on'") && err.contains("boolean"), "{err}");
    }

    #[test]
    fn test_join_empty_collection_rejected() {
        let lua = Lua::new();
        let err = field_with(&lua, |f| {
            f.set("type", "join").unwrap();
            f.set("collection", "").unwrap();
            f.set("on", "author").unwrap();
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("non-empty"), "{err}");
    }

    #[test]
    fn test_join_valid_config_parses() {
        let lua = Lua::new();
        field_with(&lua, |f| {
            f.set("type", "join").unwrap();
            f.set("collection", "posts").unwrap();
            f.set("on", "author").unwrap();
        })
        .expect("valid join must parse");
    }

    /// A Join is virtual/read-only, so `required` / `localized` /
    /// `required_locales` are meaningless and rejected at load (a
    /// `localized + required` Join previously wedged non-draft writes).
    fn join_with_flag(set_flag: impl Fn(&Table)) -> String {
        let lua = Lua::new();
        field_with(&lua, |f| {
            f.set("type", "join").unwrap();
            f.set("collection", "posts").unwrap();
            f.set("on", "author").unwrap();
            set_flag(f);
        })
        .unwrap_err()
        .to_string()
    }

    #[test]
    fn test_join_required_flag_rejected() {
        let err = join_with_flag(|f| f.set("required", true).unwrap());
        assert!(
            err.contains("required") && err.contains("meaningless"),
            "{err}"
        );
    }

    #[test]
    fn test_join_localized_flag_rejected() {
        let err = join_with_flag(|f| f.set("localized", true).unwrap());
        assert!(
            err.contains("localized") && err.contains("meaningless"),
            "{err}"
        );
    }

    #[test]
    fn test_join_required_locales_rejected() {
        let err = join_with_flag(|f| f.set("required_locales", "all").unwrap());
        assert!(
            err.contains("required_locales") && err.contains("meaningless"),
            "{err}"
        );
    }

    // ── date-bound strictness ───────────────────────────────────────────

    /// Regression: `min_date`/`max_date` used to silently drop non-string
    /// values and accept arbitrary strings — the runtime compares the bound
    /// lexically against the value's date part, so a malformed bound never
    /// (or always) matched. Wrong type, bad format, and inverted ranges are
    /// now load-time errors.
    #[test]
    fn test_date_bound_non_string_rejected() {
        let lua = Lua::new();
        let err = field_with(&lua, |f| {
            f.set("type", "date").unwrap();
            f.set("min_date", 2024i64).unwrap();
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("min_date"), "{err}");
    }

    #[test]
    fn test_date_bound_bad_format_rejected() {
        let lua = Lua::new();
        let err = field_with(&lua, |f| {
            f.set("type", "date").unwrap();
            f.set("max_date", "01.02.2024").unwrap();
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("YYYY-MM-DD"), "{err}");
    }

    #[test]
    fn test_date_bounds_inverted_range_rejected() {
        let lua = Lua::new();
        let err = field_with(&lua, |f| {
            f.set("type", "date").unwrap();
            f.set("min_date", "2024-12-31").unwrap();
            f.set("max_date", "2024-01-01").unwrap();
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("after max_date"), "{err}");
    }

    #[test]
    fn test_date_bounds_valid_range_parses() {
        let lua = Lua::new();
        field_with(&lua, |f| {
            f.set("type", "date").unwrap();
            f.set("min_date", "2024-01-01").unwrap();
            f.set("max_date", "2024-12-31").unwrap();
        })
        .expect("valid date bounds must parse");
    }
}
