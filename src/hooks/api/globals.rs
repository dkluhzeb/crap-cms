//! Register `crap.globals` — define, config.get, config.list.

use anyhow::Result;
use mlua::{Lua, Table, Value};

use crate::core::SharedRegistry;
use super::parse::parse_global_definition;
use super::serializers::global_config_to_lua;

pub(super) fn register_globals(lua: &Lua, crap: &Table, registry: SharedRegistry) -> Result<()> {
    let globals_table = lua.create_table()?;
    let reg_clone = registry.clone();
    let define_global = lua.create_function(move |lua, (slug, config): (String, Table)| {
        let def = parse_global_definition(lua, &slug, &config)
            .map_err(|e| mlua::Error::RuntimeError(format!(
                "Failed to parse global '{}': {}", slug, e
            )))?;
        let mut reg = reg_clone.write()
            .map_err(|e| mlua::Error::RuntimeError(format!("Registry lock poisoned: {}", e)))?;
        reg.register_global(def);
        Ok(())
    })?;
    globals_table.set("define", define_global)?;

    let reg_clone = registry.clone();
    let get_global = lua.create_function(move |lua, slug: String| -> mlua::Result<Value> {
        let reg = reg_clone.read()
            .map_err(|e| mlua::Error::RuntimeError(format!("Registry lock poisoned: {}", e)))?;
        match reg.get_global(&slug) {
            Some(def) => Ok(Value::Table(global_config_to_lua(lua, def)?)),
            None => Ok(Value::Nil),
        }
    })?;
    let globals_config_table = lua.create_table()?;
    globals_config_table.set("get", get_global)?;

    let reg_clone = registry.clone();
    let list_globals = lua.create_function(move |lua, ()| -> mlua::Result<Table> {
        let reg = reg_clone.read()
            .map_err(|e| mlua::Error::RuntimeError(format!("Registry lock poisoned: {}", e)))?;
        let map = lua.create_table()?;
        for (slug, def) in reg.globals.iter() {
            map.set(slug.as_str(), global_config_to_lua(lua, def)?)?;
        }
        Ok(map)
    })?;
    globals_config_table.set("list", list_globals)?;
    globals_table.set("config", globals_config_table)?;

    crap.set("globals", globals_table)?;
    Ok(())
}
