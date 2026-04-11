//! Register `crap.collections` — define, config.get, config.list.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table, Value};

use super::serializers::collection_config_to_lua;

use crate::{core::SharedRegistry, hooks::api::parse::parse_collection_definition};

/// Register `crap.collections.define`, `crap.collections.config.get`, and `crap.collections.config.list`.
pub(super) fn register_collections(
    lua: &Lua,
    crap: &Table,
    registry: SharedRegistry,
) -> Result<()> {
    let collections_table = lua.create_table()?;

    let reg = registry.clone();
    collections_table.set(
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

    collections_table.set("config", config_table)?;
    crap.set("collections", collections_table)?;

    Ok(())
}

/// Parse and register a collection definition.
fn define(lua: &Lua, reg: &SharedRegistry, slug: &str, config: &Table) -> mlua::Result<()> {
    let def = parse_collection_definition(lua, slug, config)
        .map_err(|e| RuntimeError(format!("Failed to parse collection '{slug}': {e}")))?;

    reg.write()
        .map_err(|e| RuntimeError(format!("Registry lock poisoned: {e:#}")))?
        .register_collection(def);

    Ok(())
}

/// Get a single collection config as a Lua table.
fn get(lua: &Lua, reg: &SharedRegistry, slug: &str) -> mlua::Result<Value> {
    let reg = reg
        .read()
        .map_err(|e| RuntimeError(format!("Registry lock poisoned: {e:#}")))?;

    match reg.get_collection(slug) {
        Some(def) => Ok(Value::Table(collection_config_to_lua(lua, def)?)),
        None => Ok(Value::Nil),
    }
}

/// List all collection configs as a Lua table.
fn list(lua: &Lua, reg: &SharedRegistry) -> mlua::Result<Table> {
    let reg = reg
        .read()
        .map_err(|e| RuntimeError(format!("Registry lock poisoned: {e:#}")))?;

    let map = lua.create_table()?;

    for (slug, def) in reg.collections.iter() {
        map.set(slug.as_ref(), collection_config_to_lua(lua, def)?)?;
    }

    Ok(map)
}
