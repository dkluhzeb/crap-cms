//! Register `crap.globals` — define, config.get, config.list.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table, Value};

use super::serializers::global_config_to_lua;

use crate::{
    core::SharedRegistry,
    hooks::{api::parse::parse_global_definition, lifecycle::InitPhase},
};

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
    // Globals drive single-row table creation, admin route registration,
    // and live-event wiring — all of which run once at startup. A NEW
    // runtime registration would land in `SharedRegistry` without the
    // backing row or the admin route; later reads error confusingly.
    //
    // Re-defining an EXISTING slug at runtime is the documented
    // `config.get → modify → define` round-trip pattern — accepted with
    // the same caveats as `crap.collections.define`.
    if lua.app_data_ref::<InitPhase>().is_none() {
        let already_registered = reg
            .read()
            .map_err(|e| RuntimeError(format!("Registry lock poisoned: {e:#}")))?
            .get_global(slug)
            .is_some();

        if !already_registered {
            return Err(RuntimeError(
                "crap.globals.define must be called from a definition file or init.lua \
                 for a NEW global — runtime registration does not create the row or admin \
                 routes. Re-defining an already-registered global is allowed."
                    .into(),
            ));
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;
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
        register_globals(&lua, &crap, registry.clone()).unwrap();
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

    /// Regression: re-defining an EXISTING global at runtime must
    /// succeed — same `config.get → modify → define` round-trip pattern
    /// as `crap.collections.define`. Without this exception the
    /// `tests/lua_api_filters.rs::globals_*_redefine` tests panic.
    #[test]
    fn redefine_existing_global_at_runtime_is_allowed() {
        use crate::core::collection::GlobalDefinition;

        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        let registry: SharedRegistry = Arc::new(RwLock::new(Registry::new()));
        register_globals(&lua, &crap, registry.clone()).unwrap();
        lua.globals().set("crap", crap).unwrap();

        {
            let mut reg = registry.write().unwrap();
            reg.register_global(GlobalDefinition::new("settings"));
        }

        lua.load(r#"crap.globals.define("settings", {})"#)
            .exec()
            .expect("redefining an existing global at runtime must succeed");

        let reg = registry.read().unwrap();
        assert!(
            reg.get_global("settings").is_some(),
            "global must remain registered after redefine",
        );
    }
}
