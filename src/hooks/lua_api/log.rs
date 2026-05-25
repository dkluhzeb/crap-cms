//! Register `crap.log` — info, warn, error.
//!
//! During VM pool creation, runner VMs (vm-1, vm-2, ...) re-execute init.lua
//! and collection loading. To avoid flooding the log with N duplicates of every
//! `crap.log.info()` call, runner VM messages are demoted to `debug`. Only the
//! init VM logs at the requested level.

use anyhow::Result;
use mlua::{Lua, Result as LuaResult};
use tracing::{debug, error, info, warn};

use crate::hooks::lua_api::VmLabel;
use crate::typegen::lua::{LuaFnSpec, LuaParam, lua_fn, lua_table};

/// Log an info-level message.
#[lua_fn(path = "crap.log.info")]
fn log_info(lua: &Lua, #[lua(doc = "Log message.")] msg: String) -> LuaResult<()> {
    let label = vm_label(lua);
    if is_init_vm(lua) {
        info!("[lua:{label}] {msg}");
    } else {
        debug!("[lua:{label}] {msg}");
    }
    Ok(())
}

/// Log a warning-level message.
#[lua_fn(path = "crap.log.warn")]
fn log_warn(lua: &Lua, #[lua(doc = "Log message.")] msg: String) -> LuaResult<()> {
    let label = vm_label(lua);
    warn!("[lua:{label}] {msg}");
    Ok(())
}

/// Log an error-level message.
#[lua_fn(path = "crap.log.error")]
fn log_error(lua: &Lua, #[lua(doc = "Log message.")] msg: String) -> LuaResult<()> {
    let label = vm_label(lua);
    error!("[lua:{label}] {msg}");
    Ok(())
}

lua_table! {
    name: crap_log,
    path: "crap.log",
    state: (),
    header: "Structured logging (maps to Rust tracing).",
    fns: [log_info, log_warn, log_error],
}

/// Register `crap.log` on the parent `crap` table (must already be in
/// globals — `register_api` sets it up-front for this reason).
pub(super) fn register_log(lua: &Lua) -> Result<()> {
    register_crap_log(lua, ())?;
    Ok(())
}

/// Get the VM label from Lua app data, defaulting to `"lua"`.
fn vm_label(lua: &Lua) -> String {
    lua.app_data_ref::<VmLabel>()
        .map_or_else(|| "lua".into(), |l| l.0.clone())
}

/// Whether this VM is the init VM (logs at requested level) or a runner VM
/// (logs demoted to debug to avoid N duplicates during pool creation).
fn is_init_vm(lua: &Lua) -> bool {
    lua.app_data_ref::<VmLabel>().is_some_and(|l| l.0 == "init")
}
