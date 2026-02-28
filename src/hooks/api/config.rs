//! `crap.config` and `crap.locale` namespaces — read-only config access.

use anyhow::Result;
use mlua::{Lua, Table, Value};

use crate::config::CrapConfig;

use super::json_to_lua;

/// Register `crap.config` — read-only config access with dot notation.
pub(super) fn register_config(lua: &Lua, crap: &Table, config: &CrapConfig) -> Result<()> {
    let config_table = lua.create_table()?;
    let config_json = serde_json::to_value(config)
        .map_err(|e| anyhow::anyhow!("Failed to serialize config: {}", e))?;
    let config_lua = json_to_lua(lua, &config_json)?;
    lua.globals().set("_crap_config", config_lua)?;

    let config_get_fn = lua.create_function(|lua, key: String| -> mlua::Result<Value> {
        let config_val: Value = lua.globals().get("_crap_config")?;
        let mut current = config_val;
        for part in key.split('.') {
            match current {
                Value::Table(tbl) => {
                    current = tbl.get(part)?;
                }
                _ => return Ok(Value::Nil),
            }
        }
        Ok(current)
    })?;
    config_table.set("get", config_get_fn)?;
    crap.set("config", config_table)?;
    Ok(())
}

/// Register `crap.locale` — locale configuration access.
pub(super) fn register_locale(lua: &Lua, crap: &Table, config: &CrapConfig) -> Result<()> {
    let locale_table = lua.create_table()?;

    let default_locale = config.locale.default_locale.clone();
    let get_default_fn = lua.create_function(move |_, ()| -> mlua::Result<String> {
        Ok(default_locale.clone())
    })?;
    locale_table.set("get_default", get_default_fn)?;

    let locales = config.locale.locales.clone();
    let get_all_fn = lua.create_function(move |lua, ()| -> mlua::Result<Table> {
        let tbl = lua.create_table()?;
        for (i, l) in locales.iter().enumerate() {
            tbl.set(i + 1, l.as_str())?;
        }
        Ok(tbl)
    })?;
    locale_table.set("get_all", get_all_fn)?;

    let enabled = config.locale.is_enabled();
    let is_enabled_fn = lua.create_function(move |_, ()| -> mlua::Result<bool> {
        Ok(enabled)
    })?;
    locale_table.set("is_enabled", is_enabled_fn)?;

    crap.set("locale", locale_table)?;
    Ok(())
}
