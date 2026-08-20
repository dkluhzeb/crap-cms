//! Top-level CRUD function registration for `crap.collections`, `crap.globals`, and `crap.jobs`.

use std::sync::Arc;

use anyhow::Result;
use mlua::{Lua, Table};

use crate::{
    config::{DepthConfig, JobsConfig, LocaleConfig, PaginationConfig, PasswordPolicy},
    core::Registry,
};

use super::{collection, globals, jobs};

/// Runtime config bundle threaded through the CRUD-function registration pass.
///
/// The registry is an `Arc<Registry>` (snapshot, cheaply cloned per call);
/// configs are borrowed because they're long-lived startup state.
struct CrudRegisterCtx<'a> {
    registry: &'a Arc<Registry>,
    locale_config: &'a LocaleConfig,
    pagination_config: &'a PaginationConfig,
    depth_config: &'a DepthConfig,
    /// `server.bulk_max_documents` — max docs a bulk update/delete may match
    /// (0 = unlimited), enforced in the service layer.
    bulk_max_documents: i64,
    /// `auth.password_policy` — threaded onto write contexts so the service
    /// create/update chokepoint enforces it on Lua writes too.
    password_policy: &'a PasswordPolicy,
}

/// Config bundle threaded into [`register_crud_functions`] — grouped so the
/// entry point stays a short `(lua, registry, config)` call.
pub(crate) struct CrudConfig<'a> {
    pub locale: &'a LocaleConfig,
    pub pagination: &'a PaginationConfig,
    pub depth: &'a DepthConfig,
    pub jobs: &'a JobsConfig,
    pub bulk_max_documents: i64,
    pub password_policy: &'a PasswordPolicy,
}

/// Register the CRUD functions on `crap.collections`, `crap.globals`, and `crap.jobs`.
///
/// They read the active connection from Lua `app_data` (set by `run_hooks_with_conn`).
/// Untestable as unit: registers Lua closures that require `TxContext` + full DB.
/// Covered by integration tests (hook CRUD operations in tests/).
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_crud_functions(
    lua: &Lua,
    registry: Arc<Registry>,
    config: &CrudConfig<'_>,
) -> Result<()> {
    let crap: Table = lua.globals().get("crap")?;
    let ctx = CrudRegisterCtx {
        registry: &registry,
        locale_config: config.locale,
        pagination_config: config.pagination,
        depth_config: config.depth,
        bulk_max_documents: config.bulk_max_documents,
        password_policy: config.password_policy,
    };

    register_collection_functions(lua, &crap, &ctx)?;
    register_global_functions(lua, &crap, &ctx)?;
    register_job_functions(lua, &crap, registry, config.jobs)?;

    Ok(())
}

/// Register `crap.collections.*` CRUD functions.
#[cfg(not(tarpaulin_include))]
fn register_collection_functions(lua: &Lua, crap: &Table, ctx: &CrudRegisterCtx<'_>) -> Result<()> {
    let CrudRegisterCtx {
        registry,
        locale_config,
        pagination_config,
        depth_config,
        bulk_max_documents,
        password_policy,
    } = *ctx;
    let collections: Table = crap.get("collections")?;

    // Read operations
    collection::read::find::register_find(
        lua,
        &collections,
        Arc::clone(registry),
        locale_config,
        pagination_config,
        depth_config,
    )?;
    collection::read::find_by_id::register_find_by_id(
        lua,
        &collections,
        Arc::clone(registry),
        locale_config,
        depth_config,
    )?;
    collection::read::count::register_count(
        lua,
        &collections,
        Arc::clone(registry),
        locale_config,
    )?;

    // Write operations
    collection::write::create::register_create(
        lua,
        &collections,
        Arc::clone(registry),
        locale_config,
        password_policy,
    )?;
    collection::write::update::register_update(
        lua,
        &collections,
        Arc::clone(registry),
        locale_config,
        password_policy,
    )?;
    collection::write::delete::register_delete(
        lua,
        &collections,
        Arc::clone(registry),
        locale_config,
    )?;
    collection::write::undelete::register_undelete(lua, &collections, Arc::clone(registry))?;
    collection::write::unpublish::register_unpublish(lua, &collections, Arc::clone(registry))?;
    collection::write::validate::register_validate(
        lua,
        &collections,
        Arc::clone(registry),
        locale_config,
    )?;

    // Bulk operations
    collection::bulk::create_many::register_create_many(
        lua,
        &collections,
        Arc::clone(registry),
        bulk_max_documents,
        password_policy,
    )?;
    collection::bulk::update_many::register_update_many(
        lua,
        &collections,
        Arc::clone(registry),
        locale_config,
        bulk_max_documents,
    )?;
    collection::bulk::delete_many::register_delete_many(
        lua,
        &collections,
        Arc::clone(registry),
        locale_config,
        bulk_max_documents,
    )?;

    // Version operations
    collection::versions::list::register_list_versions(lua, &collections, Arc::clone(registry))?;
    collection::versions::restore::register_restore_version(
        lua,
        &collections,
        Arc::clone(registry),
        locale_config,
    )?;

    // Document info
    collection::ref_count::register_ref_count(lua, &collections, Arc::clone(registry))?;

    Ok(())
}

/// Register `crap.globals.*` functions.
#[cfg(not(tarpaulin_include))]
fn register_global_functions(lua: &Lua, crap: &Table, ctx: &CrudRegisterCtx<'_>) -> Result<()> {
    let CrudRegisterCtx {
        registry,
        locale_config,
        ..
    } = *ctx;

    let globals_table: Table = crap.get("globals")?;

    globals::get::register_globals_get(lua, &globals_table, Arc::clone(registry), locale_config)?;
    globals::update::register_globals_update(
        lua,
        &globals_table,
        Arc::clone(registry),
        locale_config,
    )?;
    globals::validate::register_globals_validate(
        lua,
        &globals_table,
        Arc::clone(registry),
        locale_config,
    )?;
    globals::unpublish::register_globals_unpublish(
        lua,
        &globals_table,
        Arc::clone(registry),
        locale_config,
    )?;

    Ok(())
}

/// Register `crap.jobs.*` functions.
#[cfg(not(tarpaulin_include))]
fn register_job_functions(
    lua: &Lua,
    crap: &Table,
    registry: Arc<Registry>,
    jobs_config: &JobsConfig,
) -> Result<()> {
    let jobs: Table = crap.get("jobs")?;

    // Snapshot the queue-level retries defaults — only queues with an
    // explicit `Some(N)` make it into the map. Lookups in
    // `effective_max_attempts` use `HashMap::get(...).copied()`.
    let queue_retries = jobs_config
        .queues
        .iter()
        .filter_map(|(name, q)| q.retries.map(|r| (name.clone(), r)))
        .collect();

    let state = jobs::queue::JobsQueueState::new(registry, queue_retries);
    jobs::queue::register_jobs_queue(lua, &jobs, state)?;

    Ok(())
}
