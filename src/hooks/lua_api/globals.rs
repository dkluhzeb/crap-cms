//! Register `crap.globals` — define (init only), config.get, config.list.

use std::sync::Arc;

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table, Value};

use super::serializers::global_config_to_lua;

use crate::{
    core::{Registry, SharedRegistry},
    hooks::{lifecycle::InitPhase, lua_api::parse::parse_global_definition},
};

const DEFINE_INIT_ONLY_ERROR: &str = "crap.globals.define must be called from a definition file \
     or init.lua. To change a registered global, edit the file and restart the process.";

/// Init-time registration: registers `crap.globals.define` (write-capable),
/// `crap.globals.config.get`, and `crap.globals.config.list`.
pub(super) fn register_globals_init(
    lua: &Lua,
    crap: &Table,
    registry: SharedRegistry,
) -> Result<()> {
    let globals_table = lua.create_table()?;

    let reg = Arc::clone(&registry);
    globals_table.set(
        "define",
        lua.create_function(move |lua, (slug, config): (String, Table)| {
            define_init(lua, &reg, &slug, &config)
        })?,
    )?;

    let config_table = lua.create_table()?;

    let reg = Arc::clone(&registry);
    config_table.set(
        "get",
        lua.create_function(move |lua, slug: String| {
            let r = reg
                .read()
                .map_err(|e| RuntimeError(format!("Registry lock poisoned: {e:#}")))?;
            get(lua, &r, &slug)
        })?,
    )?;

    let reg = registry;
    config_table.set(
        "list",
        lua.create_function(move |lua, ()| {
            let r = reg
                .read()
                .map_err(|e| RuntimeError(format!("Registry lock poisoned: {e:#}")))?;
            list(lua, &r)
        })?,
    )?;

    globals_table.set("config", config_table)?;
    crap.set("globals", globals_table)?;

    Ok(())
}

/// Pool-VM registration: `define` is a no-op stub; config.get/list
/// read from the snapshot Arc. See [`register_collections_pool_init`]
/// for the rationale.
pub(super) fn register_globals_pool_init(
    lua: &Lua,
    crap: &Table,
    registry: Arc<Registry>,
) -> Result<()> {
    let globals_table = lua.create_table()?;

    globals_table.set(
        "define",
        lua.create_function(|lua, _: (String, Table)| -> mlua::Result<()> {
            if lua.app_data_ref::<InitPhase>().is_none() {
                return Err(RuntimeError(DEFINE_INIT_ONLY_ERROR.into()));
            }
            Ok(())
        })?,
    )?;

    let config_table = lua.create_table()?;

    let reg = Arc::clone(&registry);
    config_table.set(
        "get",
        lua.create_function(move |lua, slug: String| get(lua, &reg, &slug))?,
    )?;

    let reg = registry;
    config_table.set("list", lua.create_function(move |lua, ()| list(lua, &reg))?)?;

    globals_table.set("config", config_table)?;
    crap.set("globals", globals_table)?;

    Ok(())
}

/// Init-time define: parses + registers the global. The strict InitPhase
/// guard rejects any caller that landed here outside init.
fn define_init(lua: &Lua, reg: &SharedRegistry, slug: &str, config: &Table) -> mlua::Result<()> {
    if lua.app_data_ref::<InitPhase>().is_none() {
        return Err(RuntimeError(DEFINE_INIT_ONLY_ERROR.into()));
    }

    let def = parse_global_definition(lua, slug, config)
        .map_err(|e| RuntimeError(format!("Failed to parse global '{slug}': {e}")))?;

    reg.write()
        .map_err(|e| RuntimeError(format!("Registry lock poisoned: {e:#}")))?
        .register_global(def);

    Ok(())
}

/// Read a single global config as a Lua table. Shared between init and
/// runtime closures.
fn get(lua: &Lua, reg: &Registry, slug: &str) -> mlua::Result<Value> {
    match reg.get_global(slug) {
        Some(def) => Ok(Value::Table(global_config_to_lua(lua, def)?)),
        None => Ok(Value::Nil),
    }
}

/// Read all global configs as a Lua table. Shared between init and
/// runtime closures.
fn list(lua: &Lua, reg: &Registry) -> mlua::Result<Table> {
    let map = lua.create_table()?;

    for (slug, def) in reg.globals.iter() {
        map.set(&**slug, global_config_to_lua(lua, def)?)?;
    }

    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::GlobalDefinition;
    use crate::core::Registry;
    use std::sync::{Arc, RwLock};

    /// Regression: `crap.globals.define` from a runtime hook must be
    /// rejected — same reasoning as `crap.collections.define`. Without
    /// the guard, a hook can plant a global into `SharedRegistry` whose
    /// backing row never gets created and whose admin routes never wire.
    #[test]
    fn define_outside_init_phase_is_rejected() {
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        let registry: SharedRegistry = Arc::new(RwLock::new(Registry::new()));
        register_globals_init(&lua, &crap, Arc::clone(&registry)).unwrap();
        lua.globals().set("crap", crap).unwrap();
        // Note: NO `set_app_data(InitPhase)` — simulating a runtime hook.

        let err = lua
            .load(r#"crap.globals.define("settings", {})"#)
            .exec()
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("definition file") || err.contains("runtime registration"),
            "expected init-only error message, got: {err}"
        );

        let reg = registry.read().unwrap();
        assert!(
            reg.get_global("settings").is_none(),
            "global must NOT be registered when call is refused",
        );
    }

    /// `crap.globals.define` is also rejected for an EXISTING slug at
    /// runtime — the strict guard makes no new/existing distinction.
    /// Plugin code that needs to bulk-modify globals runs from
    /// `init.lua` where `InitPhase` is set.
    #[test]
    fn runtime_define_rejected_for_existing_slug() {
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        let registry: SharedRegistry = Arc::new(RwLock::new(Registry::new()));
        register_globals_init(&lua, &crap, Arc::clone(&registry)).unwrap();
        lua.globals().set("crap", crap).unwrap();

        {
            let mut reg = registry.write().unwrap();
            reg.register_global(GlobalDefinition::new("settings"));
        }

        let err = lua
            .load(r#"crap.globals.define("settings", {})"#)
            .exec()
            .expect_err("runtime define for existing slug must be rejected");
        assert!(
            err.to_string().contains("init.lua"),
            "error should mention init.lua: {err}"
        );
    }

    /// `crap.globals.define` succeeds during init phase — the canonical
    /// loading path.
    #[test]
    fn init_phase_define_succeeds() {
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        let registry: SharedRegistry = Arc::new(RwLock::new(Registry::new()));
        register_globals_init(&lua, &crap, Arc::clone(&registry)).unwrap();
        lua.globals().set("crap", crap).unwrap();

        lua.set_app_data(InitPhase);
        let r = lua.load(r#"crap.globals.define("settings", {})"#).exec();
        lua.remove_app_data::<InitPhase>();

        r.expect("init-phase define must succeed");
        assert!(registry.read().unwrap().get_global("settings").is_some());
    }
}
