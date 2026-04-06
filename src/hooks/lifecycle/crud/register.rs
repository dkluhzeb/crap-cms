//! Top-level CRUD function registration for `crap.collections`, `crap.globals`, and `crap.jobs`.

use anyhow::Result;
use mlua::{Lua, Table};

use crate::{
    config::{LocaleConfig, PaginationConfig},
    core::SharedRegistry,
};

use super::{
    count, create, delete, delete_many, find, find_by_id, globals_get, globals_update, jobs_queue,
    restore, update, update_many,
};

/// Register the CRUD functions on `crap.collections`, `crap.globals`, and `crap.jobs`.
///
/// They read the active connection from Lua app_data (set by `run_hooks_with_conn`).
/// Untestable as unit: registers Lua closures that require TxContext + full DB.
/// Covered by integration tests (hook CRUD operations in tests/).
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_crud_functions(
    lua: &Lua,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
    pagination_config: &PaginationConfig,
) -> Result<()> {
    let crap: Table = lua.globals().get("crap")?;

    register_collection_functions(lua, &crap, &registry, locale_config, pagination_config)?;
    register_global_functions(lua, &crap, &registry, locale_config)?;
    register_job_functions(lua, &crap, registry)?;

    Ok(())
}

/// Register `crap.collections.*` CRUD functions.
#[cfg(not(tarpaulin_include))]
fn register_collection_functions(
    lua: &Lua,
    crap: &Table,
    registry: &SharedRegistry,
    locale_config: &LocaleConfig,
    pagination_config: &PaginationConfig,
) -> Result<()> {
    let collections: Table = crap.get("collections")?;

    find::register_find(
        lua,
        &collections,
        registry.clone(),
        locale_config,
        pagination_config,
    )?;
    find_by_id::register_find_by_id(lua, &collections, registry.clone(), locale_config)?;
    create::register_create(lua, &collections, registry.clone(), locale_config)?;
    update::register_update(lua, &collections, registry.clone(), locale_config)?;
    delete::register_delete(lua, &collections, registry.clone(), locale_config)?;
    restore::register_restore(lua, &collections, registry.clone())?;
    count::register_count(lua, &collections, registry.clone(), locale_config)?;
    update_many::register_update_many(lua, &collections, registry.clone(), locale_config)?;
    delete_many::register_delete_many(lua, &collections, registry.clone(), locale_config)?;

    Ok(())
}

/// Register `crap.globals.*` functions.
#[cfg(not(tarpaulin_include))]
fn register_global_functions(
    lua: &Lua,
    crap: &Table,
    registry: &SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let globals_table: Table = crap.get("globals")?;

    globals_get::register_globals_get(lua, &globals_table, registry.clone(), locale_config)?;
    globals_update::register_globals_update(lua, &globals_table, registry.clone(), locale_config)?;

    Ok(())
}

/// Register `crap.jobs.*` functions.
#[cfg(not(tarpaulin_include))]
fn register_job_functions(lua: &Lua, crap: &Table, registry: SharedRegistry) -> Result<()> {
    let jobs: Table = crap.get("jobs")?;

    jobs_queue::register_jobs_queue(lua, &jobs, registry)?;

    Ok(())
}
