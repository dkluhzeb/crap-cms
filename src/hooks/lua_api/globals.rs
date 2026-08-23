//! Register `crap.globals` — define (init only), config.get, config.list.

use std::sync::Arc;

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table, Value};

use super::serializers::global_config_to_lua;
use super::utils::{registry_lock_poisoned, require_init_phase};

use crate::core::{Registry, SharedRegistry};
use crate::hooks::lua_api::parse::parse_global_definition;
use crate::typegen::lua::{LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table};

const DEFINE_INIT_ONLY_ERROR: &str = "crap.globals.define must be called from a definition file \
     or init.lua. To change a registered global, edit the file and restart the process.";

// ── crap.globals.define ──────────────────────────────────────────────

/// Define a new global. Call this in global definition files.
#[lua_fn(path = "crap.globals.define")]
fn globals_define_init(
    state: &SharedRegistry,
    lua: &Lua,
    #[lua(doc = "Unique global identifier.")] slug: String,
    #[lua(ty = "crap.GlobalConfig", doc = "Global configuration.")] config: Table,
) -> LuaResult<()> {
    require_init_phase(lua, DEFINE_INIT_ONLY_ERROR)?;
    let def = parse_global_definition(lua, &slug, &config)
        .map_err(|e| RuntimeError(format!("Failed to parse global '{slug}': {e}")))?;
    state
        .write()
        .map_err(registry_lock_poisoned)?
        .register_global(def);
    Ok(())
}

/// Pool-VM no-op variant — same `InitPhase` guard, but never writes
/// since the init VM already registered the global on the shared
/// registry.
#[lua_fn(path = "crap.globals.define")]
fn globals_define_pool(
    _state: &(),
    lua: &Lua,
    _slug: String,
    #[lua(ty = "crap.GlobalConfig")] _config: Table,
) -> LuaResult<()> {
    require_init_phase(lua, DEFINE_INIT_ONLY_ERROR)?;
    Ok(())
}

// ── crap.globals.config ──────────────────────────────────────────────

/// Get a global's current definition as a Lua table.
/// Returns the full config compatible with `define()` for round-trip editing.
#[lua_fn(
    path = "crap.globals.config.get",
    returns = "crap.GlobalConfig?",
    returns_doc = "The global config, or nil if not found."
)]
fn globals_config_get_init(
    state: &SharedRegistry,
    lua: &Lua,
    #[lua(doc = "Global slug.")] slug: String,
) -> LuaResult<Value> {
    let r = state.read().map_err(registry_lock_poisoned)?;
    config_get_impl(lua, &r, &slug)
}

/// Pool-VM variant: reads via the per-VM `Arc<Registry>` snapshot.
#[lua_fn(path = "crap.globals.config.get", returns = "crap.GlobalConfig?")]
fn globals_config_get_pool(state: &Arc<Registry>, lua: &Lua, slug: String) -> LuaResult<Value> {
    config_get_impl(lua, state, &slug)
}

/// List all registered globals as a slug-keyed table of full configs.
/// Iterate with `for slug, def in pairs(crap.globals.config.list()) do ... end`.
#[lua_fn(
    path = "crap.globals.config.list",
    returns = "table<string, crap.GlobalConfig>",
    returns_doc = "Slug -> global config map."
)]
fn globals_config_list_init(state: &SharedRegistry, lua: &Lua) -> LuaResult<Table> {
    let r = state.read().map_err(registry_lock_poisoned)?;
    config_list_impl(lua, &r)
}

/// Pool-VM variant of `crap.globals.config.list`.
#[lua_fn(
    path = "crap.globals.config.list",
    returns = "table<string, crap.GlobalConfig>"
)]
fn globals_config_list_pool(state: &Arc<Registry>, lua: &Lua) -> LuaResult<Table> {
    config_list_impl(lua, state)
}

// ── lua_table! groupings ─────────────────────────────────────────────

lua_table! {
    name: crap_globals_init_root,
    path: "crap.globals",
    state: SharedRegistry,
    header: "Global (singleton document) definition and runtime operations.",
    fns: [globals_define_init],
}

lua_table! {
    name: crap_globals_init_config,
    path: "crap.globals.config",
    state: SharedRegistry,
    header: "Schema introspection sub-table for globals.",
    fns: [globals_config_get_init, globals_config_list_init],
}

lua_table! {
    name: crap_globals_pool_root,
    path: "crap.globals",
    state: (),
    fns: [globals_define_pool],
}

lua_table! {
    name: crap_globals_pool_config,
    path: "crap.globals.config",
    state: Arc<Registry>,
    fns: [globals_config_get_pool, globals_config_list_pool],
}

// ── Registration entry points ────────────────────────────────────────

/// Init-time registration: registers `crap.globals.define` (write-capable),
/// `crap.globals.config.get`, and `crap.globals.config.list`.
pub(super) fn register_globals_init(lua: &Lua, registry: SharedRegistry) -> Result<()> {
    register_crap_globals_init_root(lua, Arc::clone(&registry))?;
    register_crap_globals_init_config(lua, registry)?;
    Ok(())
}

/// Pool-VM registration: `define` is a no-op stub; config.get/list
/// read from the per-VM `Arc<Registry>` snapshot.
pub(super) fn register_globals_pool_init(lua: &Lua, registry: Arc<Registry>) -> Result<()> {
    register_crap_globals_pool_root(lua, ())?;
    register_crap_globals_pool_config(lua, registry)?;
    Ok(())
}

// ── Shared read helpers ──────────────────────────────────────────────

/// Read a single global config as a Lua table. Shared between init and
/// pool closures.
fn config_get_impl(lua: &Lua, reg: &Registry, slug: &str) -> LuaResult<Value> {
    match reg.get_global(slug) {
        Some(def) => Ok(Value::Table(global_config_to_lua(lua, def)?)),
        None => Ok(Value::Nil),
    }
}

/// Read all global configs as a Lua table. Shared between init and
/// pool closures.
fn config_list_impl(lua: &Lua, reg: &Registry) -> LuaResult<Table> {
    let map = lua.create_table()?;
    for (slug, def) in &reg.globals {
        map.set(&**slug, global_config_to_lua(lua, def)?)?;
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::GlobalDefinition;
    use crate::core::Registry;
    use crate::hooks::lifecycle::InitPhase;
    use std::sync::{Arc, RwLock};

    fn lua_with_globals() -> (Lua, SharedRegistry) {
        let lua = Lua::new();
        lua.globals()
            .set("crap", lua.create_table().unwrap())
            .unwrap();
        let registry: SharedRegistry = Arc::new(RwLock::new(Registry::new()));
        register_globals_init(&lua, Arc::clone(&registry)).unwrap();
        (lua, registry)
    }

    /// Regression: `crap.globals.define` from a runtime hook must be
    /// rejected — same reasoning as `crap.collections.define`. Without
    /// the guard, a hook can plant a global into `SharedRegistry` whose
    /// backing row never gets created and whose admin routes never wire.
    #[test]
    fn define_outside_init_phase_is_rejected() {
        let (lua, registry) = lua_with_globals();
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
        let (lua, registry) = lua_with_globals();

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
        let (lua, registry) = lua_with_globals();

        lua.set_app_data(InitPhase);
        let r = lua.load(r#"crap.globals.define("settings", {})"#).exec();
        lua.remove_app_data::<InitPhase>();

        r.expect("init-phase define must succeed");
        assert!(registry.read().unwrap().get_global("settings").is_some());
    }
}
