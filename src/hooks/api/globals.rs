//! Register `crap.globals` — define, config.get, config.list.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table, Value};

use super::serializers::global_config_to_lua;

use crate::{core::SharedRegistry, hooks::api::parse::parse_global_definition};

/// Register `crap.globals.define`, `crap.globals.config.get`, and `crap.globals.config.list`.
pub(super) fn register_globals(lua: &Lua, crap: &Table, registry: SharedRegistry) -> Result<()> {
    let globals_table = lua.create_table()?;

    let reg = registry.clone();
    globals_table.set(
        "define",
        lua.create_function(move |lua, (slug, config): (String, Table)| {
            define(lua, &reg, &slug, &config)
        })?,
    )?;

    let config_table = lua.create_table()?;

    let reg = registry.clone();
    config_table.set(
        "get",
        lua.create_function(move |lua, slug: String| get(lua, &reg, &slug))?,
    )?;

    let reg = registry.clone();
    config_table.set("list", lua.create_function(move |lua, ()| list(lua, &reg))?)?;

    globals_table.set("config", config_table)?;
    crap.set("globals", globals_table)?;

    Ok(())
}

/// Parse and register a global definition.
fn define(lua: &Lua, reg: &SharedRegistry, slug: &str, config: &Table) -> mlua::Result<()> {
    let def = parse_global_definition(lua, slug, config)
        .map_err(|e| RuntimeError(format!("Failed to parse global '{slug}': {e}")))?;

    reg.write()
        .map_err(|e| RuntimeError(format!("Registry lock poisoned: {e:#}")))?
        .register_global(def);

    Ok(())
}

/// Get a single global config as a Lua table.
fn get(lua: &Lua, reg: &SharedRegistry, slug: &str) -> mlua::Result<Value> {
    let reg = reg
        .read()
        .map_err(|e| RuntimeError(format!("Registry lock poisoned: {e:#}")))?;

    match reg.get_global(slug) {
        Some(def) => Ok(Value::Table(global_config_to_lua(lua, def)?)),
        None => Ok(Value::Nil),
    }
}

/// List all global configs as a Lua table.
fn list(lua: &Lua, reg: &SharedRegistry) -> mlua::Result<Table> {
    let reg = reg
        .read()
        .map_err(|e| RuntimeError(format!("Registry lock poisoned: {e:#}")))?;

    let map = lua.create_table()?;

    for (slug, def) in reg.globals.iter() {
        map.set(&**slug, global_config_to_lua(lua, def)?)?;
    }

    Ok(map)
}
