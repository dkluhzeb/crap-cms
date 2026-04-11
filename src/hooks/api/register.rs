//! Registers the full `crap.*` Lua API namespace.

use anyhow::{Context as _, Result};
use mlua::Lua;

use crate::{config::CrapConfig, core::SharedRegistry};

use super::{
    access::register_access,
    auth::register_auth,
    collections::register_collections,
    config::{register_config, register_locale},
    crypto::register_crypto,
    email::register_email,
    env::register_env,
    fields::register_fields,
    globals::register_globals,
    hooks::register_hooks,
    http::register_http,
    jobs::register_jobs,
    log::register_log,
    richtext::register_richtext,
    schema::register_schema,
    utils::{load_lua_helpers, register_util},
};

/// Register the `crap` global table with sub-tables for collections, globals, log, util,
/// auth, env, http, config, crypto, schema, hooks, jobs, email, richtext, fields.
pub fn register_api(lua: &Lua, registry: SharedRegistry, config: &CrapConfig) -> Result<()> {
    let crap = lua.create_table().context("Failed to create crap table")?;

    register_collections(lua, &crap, registry.clone())?;
    register_globals(lua, &crap, registry.clone())?;
    register_log(lua, &crap)?;
    register_util(lua, &crap)?;
    register_crypto(lua, &crap, config.auth.secret.as_ref())?;
    register_schema(lua, &crap, registry.clone())?;
    register_hooks(lua, &crap)?;
    register_auth(lua, &crap)?;
    register_access(lua, &crap, registry.clone())?;
    register_env(lua, &crap)?;
    register_http(
        lua,
        &crap,
        config.hooks.allow_private_networks,
        config.hooks.http_max_response_bytes,
    )?;
    register_config(lua, &crap, config)?;
    register_locale(lua, &crap, config)?;
    register_jobs(lua, &crap, registry.clone())?;
    register_email(lua, &crap, config)?;
    register_richtext(lua, &crap, registry.clone())?;
    register_fields(lua, &crap)?;

    lua.globals().set("crap", crap)?;

    // Load pure Lua helpers onto crap.util (after crap global is set)
    load_lua_helpers(lua)?;

    Ok(())
}
