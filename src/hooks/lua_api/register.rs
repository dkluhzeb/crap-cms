//! Registers the full `crap.*` Lua API namespace.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use mlua::Lua;

use crate::{
    config::CrapConfig,
    core::{Registry, SharedRegistry},
};

use super::{
    access::{register_access_init, register_access_pool},
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
    schema::{register_schema_init, register_schema_pool},
    template_data::register_template_data,
    utils::{load_lua_helpers, register_util},
};

/// Register the `crap` global table for the **`init_lua`** VM — the single
/// VM that owns the writeable `SharedRegistry` and processes every
/// `<config_dir>/{collections,globals,jobs}/*.lua` plus `init.lua`.
/// Definition writes here are real (`register_collection`,
/// `register_global`, `register_job`, `register_richtext_node`).
///
/// # Errors
///
/// Returns an error if any sub-module's Lua registration fails (e.g. a Lua
/// runtime error setting up the global table).
pub fn register_api(lua: &Lua, registry: &SharedRegistry, config: &CrapConfig) -> Result<()> {
    let crap = lua.create_table().context("Failed to create crap table")?;

    // Set `crap` in globals up-front so macro-generated registrations
    // (via `ensure_table` walking from globals) can find/extend it.
    // Existing `register_X(lua, &crap, ...)` calls still mutate the same
    // Lua table — `crap` is reference-counted, both handles point to the
    // same value.
    lua.globals().set("crap", crap.clone())?;

    register_collections_init(lua, Arc::clone(registry))?;
    register_globals_init(lua, Arc::clone(registry))?;
    register_common(lua, registry, config)?;
    register_jobs_init(lua, Arc::clone(registry))?;
    register_email(lua, config)?;
    register_richtext_init(lua, Arc::clone(registry))?;
    register_fields(lua)?;
    register_template_data(lua)?;
    register_pages(lua)?;

    // Load pure Lua helpers onto crap.util (after crap global is set)
    load_lua_helpers(lua)?;

    Ok(())
}

/// Register the `crap` global table for **pool VMs** — each holds an
/// `Arc<Registry>` snapshot. Pool VMs re-run `init.lua` and
/// `jobs/*.lua` (to populate per-VM Lua state: hook functions,
/// richtext renderers, handler modules, etc.) but skip
/// `collections/` and `globals/` files. Definition writes here are
/// no-ops — the `init_lua` VM already populated the registry; pool VMs
/// only need the per-VM Lua-side side effects.
///
/// # Errors
///
/// Returns an error if any sub-module's Lua registration fails.
pub fn register_api_pool_init(
    lua: &Lua,
    registry: Arc<Registry>,
    config: &CrapConfig,
) -> Result<()> {
    let crap = lua.create_table().context("Failed to create crap table")?;

    // Set `crap` in globals up-front — see [`register_api`] for the
    // rationale (macro-generated registrations walk from globals).
    lua.globals().set("crap", crap.clone())?;

    register_collections_pool_init(lua, Arc::clone(&registry))?;
    register_globals_pool_init(lua, Arc::clone(&registry))?;
    // register_access and register_schema are generic over
    // RegistryRead — pass the Arc<Registry> directly. No lock per
    // read.
    register_common_with_arc(lua, &registry, config)?;
    register_jobs_pool_init(lua, Arc::clone(&registry))?;
    register_email(lua, config)?;
    register_richtext_pool_init(lua, registry)?;
    register_fields(lua)?;
    register_template_data(lua)?;
    register_pages(lua)?;

    load_lua_helpers(lua)?;

    Ok(())
}

/// Common register calls for the `init_lua` VM — passes `SharedRegistry`
/// to access/schema (locks per read; fine since `init_lua` is single-VM
/// and the writes happen on the same thread). Every sub-module now
/// walks `crap` from globals via `ensure_table`, so this fn no longer
/// needs the parent table reference.
fn register_common(lua: &Lua, registry: &SharedRegistry, config: &CrapConfig) -> Result<()> {
    register_log(lua)?;
    register_util(lua)?;
    register_crypto(lua, config.auth.secret.as_ref())?;
    register_schema_init(lua, Arc::clone(registry))?;
    register_hooks(lua)?;
    register_auth(lua)?;
    register_access_init(lua, Arc::clone(registry))?;
    register_env(lua)?;
    register_http(
        lua,
        config.hooks.allow_private_networks,
        config.hooks.http_max_response_bytes,
    )?;
    register_config(lua, config)?;
    register_locale(lua, config)?;
    Ok(())
}

/// Common register calls for pool VMs — passes `Arc<Registry>` to
/// access/schema for lock-free reads. `register_schema_pool` and
/// `register_access_pool` are the concrete-state pool variants of
/// the respective registrations. As in `register_common`, every
/// sub-module walks `crap` from globals so the parent table
/// reference is no longer needed here.
fn register_common_with_arc(
    lua: &Lua,
    registry: &Arc<Registry>,
    config: &CrapConfig,
) -> Result<()> {
    register_log(lua)?;
    register_util(lua)?;
    register_crypto(lua, config.auth.secret.as_ref())?;
    register_schema_pool(lua, Arc::clone(registry))?;
    register_hooks(lua)?;
    register_auth(lua)?;
    register_access_pool(lua, Arc::clone(registry))?;
    register_env(lua)?;
    register_http(
        lua,
        config.hooks.allow_private_networks,
        config.hooks.http_max_response_bytes,
    )?;
    register_config(lua, config)?;
    register_locale(lua, config)?;
    Ok(())
}
