//! Register `crap.collections` — define (init only), config.get, config.list.

use std::sync::Arc;

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table, Value};

use super::serializers::collection_config_to_lua;
use super::utils::{registry_lock_poisoned, require_init_phase};

use crate::core::{Registry, SharedRegistry};
use crate::hooks::lua_api::parse::parse_collection_definition;
use crate::typegen::lua::{LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table};

const DEFINE_INIT_ONLY_ERROR: &str = "crap.collections.define must be called from a definition \
     file or init.lua. To change a registered collection, edit the file and restart the process.";

// ── crap.collections.define ──────────────────────────────────────────

/// Define a new collection. Call this in collection definition files.
#[lua_fn(path = "crap.collections.define")]
fn collections_define_init(
    state: &SharedRegistry,
    lua: &Lua,
    #[lua(doc = "Unique collection identifier (used in URLs and DB).")] slug: String,
    #[lua(ty = "crap.CollectionConfig", doc = "Collection configuration.")] config: Table,
) -> LuaResult<()> {
    require_init_phase(lua, DEFINE_INIT_ONLY_ERROR)?;

    let def = parse_collection_definition(lua, &slug, &config)
        .map_err(|e| RuntimeError(format!("Failed to parse collection '{slug}': {e}")))?;
    state
        .write()
        .map_err(registry_lock_poisoned)?
        .register_collection(def);
    Ok(())
}

/// Pool-VM no-op variant — same `InitPhase` guard, but never writes
/// since the init VM already registered the collection on the shared
/// registry. Pool VMs DO re-run definition files (to populate per-VM
/// Lua-side state) but skip the registry write.
#[lua_fn(path = "crap.collections.define")]
fn collections_define_pool(
    _state: &(),
    lua: &Lua,
    _slug: String,
    #[lua(ty = "crap.CollectionConfig")] _config: Table,
) -> LuaResult<()> {
    require_init_phase(lua, DEFINE_INIT_ONLY_ERROR)?;
    Ok(())
}

// ── crap.collections.config ──────────────────────────────────────────

/// Get a collection's current definition as a Lua table.
/// Returns the full config compatible with `define()` for round-trip editing.
#[lua_fn(
    path = "crap.collections.config.get",
    returns = "crap.CollectionConfig?",
    returns_doc = "The collection config, or nil if not found."
)]
fn collections_config_get_init(
    state: &SharedRegistry,
    lua: &Lua,
    #[lua(doc = "Collection slug.")] slug: String,
) -> LuaResult<Value> {
    let r = state.read().map_err(registry_lock_poisoned)?;
    config_get_impl(lua, &r, &slug)
}

/// Pool-VM variant: reads via the per-VM `Arc<Registry>` snapshot
/// without taking the lock.
#[lua_fn(
    path = "crap.collections.config.get",
    returns = "crap.CollectionConfig?"
)]
fn collections_config_get_pool(state: &Arc<Registry>, lua: &Lua, slug: String) -> LuaResult<Value> {
    config_get_impl(lua, state, &slug)
}

/// List all registered collections as a slug-keyed table of full configs.
/// Iterate with `for slug, def in pairs(crap.collections.config.list()) do ... end`.
#[lua_fn(
    path = "crap.collections.config.list",
    returns = "table<string, crap.CollectionConfig>",
    returns_doc = "Slug -> collection config map."
)]
fn collections_config_list_init(state: &SharedRegistry, lua: &Lua) -> LuaResult<Table> {
    let r = state.read().map_err(registry_lock_poisoned)?;
    config_list_impl(lua, &r)
}

/// Pool-VM variant of `crap.collections.config.list`.
#[lua_fn(
    path = "crap.collections.config.list",
    returns = "table<string, crap.CollectionConfig>"
)]
fn collections_config_list_pool(state: &Arc<Registry>, lua: &Lua) -> LuaResult<Table> {
    config_list_impl(lua, state)
}

// ── lua_table! groupings ─────────────────────────────────────────────

lua_table! {
    name: crap_collections_init_root,
    path: "crap.collections",
    state: SharedRegistry,
    header: "Collection definition and runtime CRUD operations.",
    fns: [collections_define_init],
}

lua_table! {
    name: crap_collections_init_config,
    path: "crap.collections.config",
    state: SharedRegistry,
    header: "Schema introspection sub-table for collections.",
    fns: [collections_config_get_init, collections_config_list_init],
}

lua_table! {
    name: crap_collections_pool_root,
    path: "crap.collections",
    state: (),
    fns: [collections_define_pool],
}

lua_table! {
    name: crap_collections_pool_config,
    path: "crap.collections.config",
    state: Arc<Registry>,
    fns: [collections_config_get_pool, collections_config_list_pool],
}

// ── Registration entry points ────────────────────────────────────────

/// Init-time registration: registers `crap.collections.define` (write-capable),
/// `crap.collections.config.get`, and `crap.collections.config.list`.
/// Used by the init-phase Lua VM that loads `<config_dir>/collections/`,
/// `<config_dir>/init.lua`, and any files they `require`.
pub(super) fn register_collections_init(lua: &Lua, registry: SharedRegistry) -> Result<()> {
    register_crap_collections_init_root(lua, Arc::clone(&registry))?;
    register_crap_collections_init_config(lua, registry)?;
    Ok(())
}

/// Pool-VM registration: `define` is a no-op stub. Pool VMs DO re-run
/// definition files (to populate per-VM Lua-side state) but the
/// registry write lands on the init VM only — the pool VMs share the
/// `Arc<Registry>` snapshot for reads. Runtime calls (no `InitPhase`)
/// still error via the strict guard.
pub(super) fn register_collections_pool_init(lua: &Lua, registry: Arc<Registry>) -> Result<()> {
    register_crap_collections_pool_root(lua, ())?;
    register_crap_collections_pool_config(lua, registry)?;
    Ok(())
}

// ── Shared read helpers ──────────────────────────────────────────────

/// Read a single collection config as a Lua table. Shared between init
/// and pool closures.
fn config_get_impl(lua: &Lua, reg: &Registry, slug: &str) -> LuaResult<Value> {
    match reg.get_collection(slug) {
        Some(def) => Ok(Value::Table(collection_config_to_lua(lua, def)?)),
        None => Ok(Value::Nil),
    }
}

/// Read all collection configs as a Lua table. Shared between init and
/// pool closures.
fn config_list_impl(lua: &Lua, reg: &Registry) -> LuaResult<Table> {
    let map = lua.create_table()?;
    for (slug, def) in &reg.collections {
        map.set(slug.as_ref(), collection_config_to_lua(lua, def)?)?;
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::CollectionDefinition;
    use crate::core::Registry;
    use crate::hooks::lifecycle::InitPhase;
    use std::sync::{Arc, RwLock};

    fn lua_with_collections() -> (Lua, SharedRegistry) {
        let lua = Lua::new();
        lua.globals()
            .set("crap", lua.create_table().unwrap())
            .unwrap();
        let registry: SharedRegistry = Arc::new(RwLock::new(Registry::new()));
        register_collections_init(&lua, Arc::clone(&registry)).unwrap();
        (lua, registry)
    }

    /// Regression: `crap.collections.define` called outside the init phase
    /// must fail loudly. Without the guard, a runtime hook could insert a
    /// collection into `SharedRegistry` that has no backing table, no admin
    /// route, and no sidebar entry — every subsequent reference to it
    /// errors at use time with a misleading "no such table" / 404. The
    /// init-phase requirement matches the rest of the registration APIs
    /// (`crap.pages.register`, `crap.template_data.register`, etc.).
    #[test]
    fn define_outside_init_phase_is_rejected() {
        let (lua, registry) = lua_with_collections();
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
        let (lua, registry) = lua_with_collections();

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
        let (lua, registry) = lua_with_collections();

        lua.set_app_data(InitPhase);
        let r = lua.load(r#"crap.collections.define("posts", {})"#).exec();
        lua.remove_app_data::<InitPhase>();

        r.expect("init-phase define must succeed");
        assert!(registry.read().unwrap().get_collection("posts").is_some());
    }
}
