//! Register `crap.collections` — define (init only), config.get, config.list.

use std::sync::Arc;

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table, Value};

use super::serializers::collection_config_to_lua;

use crate::{
    core::{Registry, SharedRegistry},
    hooks::{lifecycle::InitPhase, lua_api::parse::parse_collection_definition},
};

const DEFINE_INIT_ONLY_ERROR: &str = "crap.collections.define must be called from a definition \
     file or init.lua. To change a registered collection, edit the file and restart the process.";

/// Init-time registration: registers `crap.collections.define` (write-capable),
/// `crap.collections.config.get`, and `crap.collections.config.list`.
/// Used by the init-phase Lua VM that loads `<config_dir>/collections/`,
/// `<config_dir>/init.lua`, and any files they `require`.
pub(super) fn register_collections_init(
    lua: &Lua,
    crap: &Table,
    registry: SharedRegistry,
) -> Result<()> {
    let collections_table = lua.create_table()?;

    let reg = Arc::clone(&registry);
    collections_table.set(
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

    collections_table.set("config", config_table)?;
    crap.set("collections", collections_table)?;

    Ok(())
}

/// Pool-VM registration: registers `crap.collections.define` as a no-op
/// stub (registry was already populated by `init_lua` before
/// `HookRunner::build`; pool VMs re-running `init.lua` and `jobs/*.lua`
/// would otherwise try to write again). Reads use the snapshot `Arc`
/// directly — no lock. Runtime calls (no `InitPhase`) error via the
/// strict guard, same as init-flavor.
pub(super) fn register_collections_pool_init(
    lua: &Lua,
    crap: &Table,
    registry: Arc<Registry>,
) -> Result<()> {
    let collections_table = lua.create_table()?;

    collections_table.set(
        "define",
        lua.create_function(|lua, _: (String, Table)| -> mlua::Result<()> {
            if lua.app_data_ref::<InitPhase>().is_none() {
                return Err(RuntimeError(DEFINE_INIT_ONLY_ERROR.into()));
            }
            // Pool VM init-phase: define is a no-op. The init_lua VM
            // already populated the registry; re-running definition
            // files here would double-write idempotently. We simply
            // skip the write.
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

    collections_table.set("config", config_table)?;
    crap.set("collections", collections_table)?;

    Ok(())
}

/// Init-time define: parses + registers the collection. The strict
/// InitPhase guard rejects any caller that landed here outside init.
fn define_init(lua: &Lua, reg: &SharedRegistry, slug: &str, config: &Table) -> mlua::Result<()> {
    if lua.app_data_ref::<InitPhase>().is_none() {
        return Err(RuntimeError(DEFINE_INIT_ONLY_ERROR.into()));
    }

    let def = parse_collection_definition(lua, slug, config)
        .map_err(|e| RuntimeError(format!("Failed to parse collection '{slug}': {e}")))?;

    reg.write()
        .map_err(|e| RuntimeError(format!("Registry lock poisoned: {e:#}")))?
        .register_collection(def);

    Ok(())
}

/// Read a single collection config as a Lua table. Shared between init
/// and runtime closures.
fn get(lua: &Lua, reg: &Registry, slug: &str) -> mlua::Result<Value> {
    match reg.get_collection(slug) {
        Some(def) => Ok(Value::Table(collection_config_to_lua(lua, def)?)),
        None => Ok(Value::Nil),
    }
}

/// Read all collection configs as a Lua table. Shared between init and
/// runtime closures.
fn list(lua: &Lua, reg: &Registry) -> mlua::Result<Table> {
    let map = lua.create_table()?;

    for (slug, def) in reg.collections.iter() {
        map.set(slug.as_ref(), collection_config_to_lua(lua, def)?)?;
    }

    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::CollectionDefinition;
    use crate::core::Registry;
    use std::sync::{Arc, RwLock};

    /// Regression: `crap.collections.define` called outside the init phase
    /// must fail loudly. Without the guard, a runtime hook could insert a
    /// collection into `SharedRegistry` that has no backing table, no admin
    /// route, and no sidebar entry — every subsequent reference to it
    /// errors at use time with a misleading "no such table" / 404. The
    /// init-phase requirement matches the rest of the registration APIs
    /// (`crap.pages.register`, `crap.template_data.register`, etc.).
    #[test]
    fn define_outside_init_phase_is_rejected() {
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        let registry: SharedRegistry = Arc::new(RwLock::new(Registry::new()));
        register_collections_init(&lua, &crap, Arc::clone(&registry)).unwrap();
        lua.globals().set("crap", crap).unwrap();
        // Note: NO `set_app_data(InitPhase)` — simulating a runtime hook.

        let err = lua
            .load(r#"crap.collections.define("posts", {})"#)
            .exec()
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("definition file") || err.contains("runtime registration"),
            "expected init-only error message, got: {err}"
        );

        // The collection MUST NOT have been registered.
        let reg = registry.read().unwrap();
        assert!(
            reg.get_collection("posts").is_none(),
            "collection must NOT be registered when call is refused",
        );
    }

    /// `crap.collections.define` is init-only at runtime — both for new
    /// and existing slugs. Plugin code that loops over
    /// `crap.collections.config.list()` and bulk-redefines runs from
    /// `init.lua` (or files it requires) where `InitPhase` is set, so
    /// the strict guard never fires for legitimate plugin code. Mirrors
    /// `crap.richtext.register_node`'s pre-existing strict guard.
    #[test]
    fn runtime_define_rejected_for_existing_slug() {
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        let registry: SharedRegistry = Arc::new(RwLock::new(Registry::new()));
        register_collections_init(&lua, &crap, Arc::clone(&registry)).unwrap();
        lua.globals().set("crap", crap).unwrap();

        // Pre-register an existing slug so we test the
        // existing-slug-at-runtime path specifically.
        {
            let mut reg = registry.write().unwrap();
            reg.register_collection(CollectionDefinition::new("posts"));
        }

        // Without InitPhase, the runtime redefine must error.
        let err = lua
            .load(r#"crap.collections.define("posts", {})"#)
            .exec()
            .expect_err("runtime define for existing slug must be rejected");
        assert!(
            err.to_string().contains("init.lua"),
            "error should mention init.lua: {err}"
        );
    }

    /// `crap.collections.define` succeeds during init phase — the
    /// canonical loading path used by `init.lua` and definition files
    /// in `<config_dir>/collections/`.
    #[test]
    fn init_phase_define_succeeds() {
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        let registry: SharedRegistry = Arc::new(RwLock::new(Registry::new()));
        register_collections_init(&lua, &crap, Arc::clone(&registry)).unwrap();
        lua.globals().set("crap", crap).unwrap();

        lua.set_app_data(InitPhase);
        let r = lua.load(r#"crap.collections.define("posts", {})"#).exec();
        lua.remove_app_data::<InitPhase>();

        r.expect("init-phase define must succeed");
        assert!(registry.read().unwrap().get_collection("posts").is_some());
    }
}
