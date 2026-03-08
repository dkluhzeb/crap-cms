//! Lua CRUD function registration — split into per-operation modules.

mod find;
mod write;
mod delete;
mod globals;

use anyhow::Result;
use mlua::Lua;

use crate::config::LocaleConfig;
use crate::core::SharedRegistry;

use super::TxContext;

/// Get the active transaction connection from Lua app_data.
/// Returns an error if called outside of `run_hooks_with_conn`.
pub(crate) fn get_tx_conn(lua: &Lua) -> mlua::Result<*const rusqlite::Connection> {
    let ctx = lua.app_data_ref::<TxContext>()
        .ok_or_else(|| mlua::Error::RuntimeError(
            "crap.collections CRUD functions are only available inside hooks \
             with transaction context (before_change, before_delete, etc.)"
                .into()
        ))?;
    Ok(ctx.0)
}

/// Register the CRUD functions on `crap.collections` and `crap.globals`.
/// They read the active connection from Lua app_data (set by `run_hooks_with_conn`).
/// Untestable as unit: registers Lua closures that require TxContext + full DB.
/// Covered by integration tests (hook CRUD operations in tests/).
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_crud_functions(
    lua: &Lua,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
    pagination_config: &crate::config::PaginationConfig,
) -> Result<()> {
    let crap: mlua::Table = lua.globals().get("crap")?;
    let collections: mlua::Table = crap.get("collections")?;

    find::register_find(lua, &collections, registry.clone(), locale_config, pagination_config)?;
    find::register_find_by_id(lua, &collections, registry.clone(), locale_config)?;
    write::register_create(lua, &collections, registry.clone(), locale_config)?;
    write::register_update(lua, &collections, registry.clone(), locale_config)?;
    delete::register_delete(lua, &collections, registry.clone())?;
    find::register_count(lua, &collections, registry.clone(), locale_config)?;
    delete::register_update_many(lua, &collections, registry.clone(), locale_config)?;
    delete::register_delete_many(lua, &collections, registry.clone(), locale_config)?;

    let globals_table: mlua::Table = crap.get("globals")?;
    globals::register_globals_get(lua, &globals_table, registry.clone(), locale_config)?;
    globals::register_globals_update(lua, &globals_table, registry.clone(), locale_config)?;

    let jobs: mlua::Table = crap.get("jobs")?;
    globals::register_jobs_queue(lua, &jobs, registry)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Lua;

    #[test]
    fn test_get_tx_conn_without_context() {
        let lua = Lua::new();
        let result = get_tx_conn(&lua);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("only available inside hooks"));
    }
}
