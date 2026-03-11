//! Registers the `crap.*` Lua API namespace (collections, globals, hooks, log, util,
//! crypto, schema).

mod auth;
mod collections;
mod config;
mod crypto;
mod email;
mod env;
mod fields;
mod globals;
mod hooks;
mod http;
mod jobs;
mod log;
pub mod parse;
pub(crate) mod richtext;
mod schema;
mod serializers;
mod util;

use anyhow::{Context as _, Result};
use mlua::Lua;
use std::path::Path;

use crate::config::CrapConfig;
use crate::core::SharedRegistry;

pub(crate) use serializers::{json_to_lua, lua_to_json};

/// Label stored in `Lua::app_data` to identify which VM is logging.
/// Init VM uses `"init"`, pool VMs use `"vm-1"`, `"vm-2"`, etc.
pub struct VmLabel(pub String);

/// Register the `crap` global table with sub-tables for collections, globals, log, util,
/// auth, env, http, config.
pub fn register_api(
    lua: &Lua,
    registry: SharedRegistry,
    _config_dir: &Path,
    config: &CrapConfig,
) -> Result<()> {
    let crap = lua.create_table().context("Failed to create crap table")?;

    collections::register_collections(lua, &crap, registry.clone())?;
    globals::register_globals(lua, &crap, registry.clone())?;
    log::register_log(lua, &crap)?;
    util::register_util(lua, &crap)?;
    crypto::register_crypto(lua, &crap, &config.auth.secret)?;
    schema::register_schema(lua, &crap, registry.clone())?;
    hooks::register_hooks(lua, &crap)?;
    auth::register_auth(lua, &crap)?;
    env::register_env(lua, &crap)?;
    http::register_http(lua, &crap)?;
    config::register_config(lua, &crap, config)?;
    config::register_locale(lua, &crap, config)?;
    jobs::register_jobs(lua, &crap, registry.clone())?;
    email::register_email(lua, &crap, config)?;
    richtext::register_richtext(lua, &crap, registry.clone())?;
    fields::register_fields(lua, &crap)?;

    lua.globals().set("crap", crap)?;

    // Load pure Lua helpers onto crap.util (after crap global is set)
    util::load_lua_helpers(lua)?;

    Ok(())
}
