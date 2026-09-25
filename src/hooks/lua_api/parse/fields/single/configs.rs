//! Type-scoped and optional config tables on a field: `join`'s target,
//! `mcp` metadata, `required_locales`.

use anyhow::{Result, bail};
use mlua::{Result as LuaResult, Table, Value};

use crate::{
    core::{FieldType, JoinConfig, McpFieldConfig, RequiredLocales},
    hooks::lua_api::parse::helpers::{deny_unknown_keys, get_string, get_table},
};

/// Build the `JoinConfig` for `FieldType::Join`. `collection` and `on` are
/// required non-empty strings — a join without them can never resolve, so a
/// missing, wrong-typed, or empty value is a load-time error instead of a
/// silently-empty config that matches nothing at populate time.
pub(super) fn parse_join(
    field_tbl: &Table,
    field_type: &FieldType,
    name: &str,
) -> Result<Option<JoinConfig>> {
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
pub(super) fn parse_mcp(field_tbl: &Table) -> Result<McpFieldConfig> {
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
    let val: Value = tbl.get("required_locales")?;
    match val {
        Value::Nil => Ok(None),
        Value::String(s) => {
            let s = s.to_str()?;
            if &*s == "all" {
                Ok(Some(RequiredLocales::All))
            } else {
                bail!("required_locales string must be \"all\", got '{}'", &*s)
            }
        }
        Value::Table(t) => {
            let locales = t
                .sequence_values::<String>()
                .collect::<LuaResult<Vec<_>>>()?;
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
