//! Register `crap.collections` — define, config.get, config.list.

use anyhow::Result;
use mlua::{Lua, Table, Value};

use crate::core::SharedRegistry;
use super::parse::parse_collection_definition;
use super::serializers::collection_config_to_lua;

pub(super) fn register_collections(lua: &Lua, crap: &Table, registry: SharedRegistry) -> Result<()> {
    let collections_table = lua.create_table()?;
    let reg_clone = registry.clone();
    let define_collection = lua.create_function(move |lua, (slug, config): (String, Table)| {
        let def = parse_collection_definition(lua, &slug, &config)
            .map_err(|e| mlua::Error::RuntimeError(format!(
                "Failed to parse collection '{}': {}", slug, e
            )))?;
        let mut reg = reg_clone.write()
            .map_err(|e| mlua::Error::RuntimeError(format!("Registry lock poisoned: {}", e)))?;
        reg.register_collection(def);
        Ok(())
    })?;
    collections_table.set("define", define_collection)?;

    let reg_clone = registry.clone();
    let get_collection = lua.create_function(move |lua, slug: String| -> mlua::Result<Value> {
        let reg = reg_clone.read()
            .map_err(|e| mlua::Error::RuntimeError(format!("Registry lock poisoned: {}", e)))?;
        match reg.get_collection(&slug) {
            Some(def) => Ok(Value::Table(collection_config_to_lua(lua, def)?)),
            None => Ok(Value::Nil),
        }
    })?;
    let collections_config_table = lua.create_table()?;
    collections_config_table.set("get", get_collection)?;

    let reg_clone = registry.clone();
    let list_collections = lua.create_function(move |lua, ()| -> mlua::Result<Table> {
        let reg = reg_clone.read()
            .map_err(|e| mlua::Error::RuntimeError(format!("Registry lock poisoned: {}", e)))?;
        let map = lua.create_table()?;
        for (slug, def) in reg.collections.iter() {
            map.set(slug.as_str(), collection_config_to_lua(lua, def)?)?;
        }
        Ok(map)
    })?;
    collections_config_table.set("list", list_collections)?;
    collections_table.set("config", collections_config_table)?;

    crap.set("collections", collections_table)?;
    Ok(())
}
