//! Registers the full `crap.*` Lua API namespace.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use mlua::Lua;

use crate::{
    config::CrapConfig,
    core::{Registry, SharedRegistry},
};

use super::{
    access::register_access,
    auth::register_auth,
    collections::{register_collections_init, register_collections_pool_init},
    config::{register_config, register_locale},
    crypto::register_crypto,
    email::register_email,
    env::register_env,
    fields::register_fields,
    globals::{register_globals_init, register_globals_pool_init},
    hooks::register_hooks,
    http::register_http,
    jobs::{register_jobs_init, register_jobs_pool_init},
    log::register_log,
    pages::register_pages,
    richtext::{register_richtext_init, register_richtext_pool_init},
    schema::register_schema,
    template_data::register_template_data,
    utils::{load_lua_helpers, register_util},
};

/// Register the `crap` global table for the **init_lua** VM — the single
/// VM that owns the writeable `SharedRegistry` and processes every
/// `<config_dir>/{collections,globals,jobs}/*.lua` plus `init.lua`.
/// Definition writes here are real (`register_collection`,
/// `register_global`, `register_job`, `register_richtext_node`).
pub fn register_api(lua: &Lua, registry: SharedRegistry, config: &CrapConfig) -> Result<()> {
    let crap = lua.create_table().context("Failed to create crap table")?;

    register_collections_init(lua, &crap, Arc::clone(&registry))?;
    register_globals_init(lua, &crap, Arc::clone(&registry))?;
    register_common(lua, &crap, &registry, config)?;
    register_jobs_init(lua, &crap, Arc::clone(&registry))?;
    register_email(lua, &crap, config)?;
    register_richtext_init(lua, &crap, Arc::clone(&registry))?;
    register_fields(lua, &crap)?;
    register_template_data(lua, &crap)?;
    register_pages(lua, &crap)?;

    lua.globals().set("crap", crap)?;

    // Load pure Lua helpers onto crap.util (after crap global is set)
    load_lua_helpers(lua)?;

    Ok(())
}

/// Register the `crap` global table for **pool VMs** — each holds an
/// `Arc<Registry>` snapshot. Pool VMs re-run `init.lua` and
/// `jobs/*.lua` (to populate per-VM Lua state: hook functions,
/// richtext renderers, handler modules, etc.) but skip
/// `collections/` and `globals/` files. Definition writes here are
/// no-ops — the init_lua VM already populated the registry; pool VMs
/// only need the per-VM Lua-side side effects.
pub fn register_api_pool_init(
    lua: &Lua,
    registry: Arc<Registry>,
    config: &CrapConfig,
) -> Result<()> {
    let crap = lua.create_table().context("Failed to create crap table")?;

    register_collections_pool_init(lua, &crap, Arc::clone(&registry))?;
    register_globals_pool_init(lua, &crap, Arc::clone(&registry))?;
    // register_access and register_schema are generic over
    // RegistryRead — pass the Arc<Registry> directly. No lock per
    // read.
    register_common_with_arc(lua, &crap, &registry, config)?;
    register_jobs_pool_init(lua, &crap, Arc::clone(&registry))?;
    register_email(lua, &crap, config)?;
    register_richtext_pool_init(lua, &crap, registry)?;
    register_fields(lua, &crap)?;
    register_template_data(lua, &crap)?;
    register_pages(lua, &crap)?;

    lua.globals().set("crap", crap)?;

    load_lua_helpers(lua)?;

    Ok(())
}

/// Common register calls for the init_lua VM — passes SharedRegistry
/// to access/schema (locks per read; fine since init_lua is single-VM
/// and the writes happen on the same thread).
fn register_common(
    lua: &Lua,
    crap: &mlua::Table,
    registry: &SharedRegistry,
    config: &CrapConfig,
) -> Result<()> {
    register_log(lua, crap)?;
    register_util(lua, crap)?;
    register_crypto(lua, crap, config.auth.secret.as_ref())?;
    register_schema(lua, crap, Arc::clone(registry))?;
    register_hooks(lua, crap)?;
    register_auth(lua, crap)?;
    register_access(lua, crap, Arc::clone(registry))?;
    register_env(lua, crap)?;
    register_http(
        lua,
        crap,
        config.hooks.allow_private_networks,
        config.hooks.http_max_response_bytes,
    )?;
    register_config(lua, crap, config)?;
    register_locale(lua, crap, config)?;
    Ok(())
}

/// Common register calls for pool VMs — passes `Arc<Registry>` to
/// access/schema for lock-free reads. Same generic register_access /
/// register_schema fns as `register_common` — just different
/// `RegistryRead` impl monomorphized in.
fn register_common_with_arc(
    lua: &Lua,
    crap: &mlua::Table,
    registry: &Arc<Registry>,
    config: &CrapConfig,
) -> Result<()> {
    register_log(lua, crap)?;
    register_util(lua, crap)?;
    register_crypto(lua, crap, config.auth.secret.as_ref())?;
    register_schema(lua, crap, Arc::clone(registry))?;
    register_hooks(lua, crap)?;
    register_auth(lua, crap)?;
    register_access(lua, crap, Arc::clone(registry))?;
    register_env(lua, crap)?;
    register_http(
        lua,
        crap,
        config.hooks.allow_private_networks,
        config.hooks.http_max_response_bytes,
    )?;
    register_config(lua, crap, config)?;
    register_locale(lua, crap, config)?;
    Ok(())
}
