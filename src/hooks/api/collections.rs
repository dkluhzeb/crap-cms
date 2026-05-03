//! Register `crap.collections` — define, config.get, config.list.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table, Value};

use super::serializers::collection_config_to_lua;

use crate::{
    core::SharedRegistry,
    hooks::{api::parse::parse_collection_definition, lifecycle::InitPhase},
};

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
    // Collections drive table creation, route registration, and admin-UI
    // wiring — all of which run once at startup from the live registry.
    // A NEW runtime registration writes into the SharedRegistry but no
    // migration runs, so the table never exists; the first read/write
    // through the new collection errors with a confusing "no such table"
    // diagnostic and the admin sidebar never gains the entry. Refuse it.
    //
    // BUT: re-defining an existing slug at runtime is a documented
    // round-trip pattern (`config.get → modify → define`) — admins use it
    // to programmatically tweak field lists, hooks, etc. The new
    // definition lands in the registry; the DB schema and admin routes
    // stay as they were. Schema-vs-registry drift on column-bearing
    // changes is a footgun the caller has to weigh, but the call itself
    // is legitimate, so accept it.
    if lua.app_data_ref::<InitPhase>().is_none() {
        let already_registered = reg
            .read()
            .map_err(|e| RuntimeError(format!("Registry lock poisoned: {e:#}")))?
            .get_collection(slug)
            .is_some();

        if !already_registered {
            return Err(RuntimeError(
                "crap.collections.define must be called from a definition file or init.lua \
                 for a NEW collection — runtime registration does not create the table \
                 or admin routes. Re-defining an already-registered collection is allowed."
                    .into(),
            ));
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;
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
        register_collections(&lua, &crap, registry.clone()).unwrap();
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

    /// Regression: re-defining an EXISTING collection at runtime must
    /// succeed. The `config.get → modify → define` round-trip is a
    /// documented Lua API — admins use it to programmatically tweak
    /// field lists, hooks, etc. The registry update lands; the DB
    /// schema and admin routes stay as they were. Schema-vs-registry
    /// drift on column-bearing changes is a footgun the caller weighs,
    /// but the call itself is legitimate. Without this exception the
    /// `tests/lua_api_filters.rs` redefine tests panic.
    #[test]
    fn redefine_existing_collection_at_runtime_is_allowed() {
        use crate::core::CollectionDefinition;

        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        let registry: SharedRegistry = Arc::new(RwLock::new(Registry::new()));
        register_collections(&lua, &crap, registry.clone()).unwrap();
        lua.globals().set("crap", crap).unwrap();

        // Pre-populate: simulate an init-time registration without
        // going through the Lua API (avoids the bigger test setup of a
        // valid Lua collection definition).
        {
            let mut reg = registry.write().unwrap();
            reg.register_collection(CollectionDefinition::new("posts"));
        }

        // Without InitPhase, the runtime redefine must succeed.
        lua.load(r#"crap.collections.define("posts", {})"#)
            .exec()
            .expect("redefining an existing collection at runtime must succeed");

        // The collection is still registered (and has been updated to
        // the new — empty — definition).
        let reg = registry.read().unwrap();
        assert!(
            reg.get_collection("posts").is_some(),
            "collection must remain registered after redefine",
        );
    }
}
