//! Register `crap.log` — info, warn, error.
//!
//! During VM pool creation, runner VMs (vm-1, vm-2, ...) re-execute init.lua
//! and collection loading. To avoid flooding the log with N duplicates of every
//! `crap.log.info()` call, runner VM messages are demoted to `debug`. Only the
//! init VM logs at the requested level.

use anyhow::Result;
use mlua::{Lua, Table};
use tracing::{debug, error, info, warn};

use crate::hooks::api::VmLabel;

/// Log messages include the VM label (e.g. `[lua:init]`, `[lua:vm-1]`).
pub(super) fn register_log(lua: &Lua, crap: &Table) -> Result<()> {
    let t = lua.create_table()?;

    t.set(
        "info",
        lua.create_function(|lua, msg: String| {
            log_info(lua, &msg);
            Ok(())
        })?,
    )?;
    t.set(
        "warn",
        lua.create_function(|lua, msg: String| {
            log_warn(lua, &msg);
            Ok(())
        })?,
    )?;
    t.set(
        "error",
        lua.create_function(|lua, msg: String| {
            log_error(lua, &msg);
            Ok(())
        })?,
    )?;

    crap.set("log", t)?;

    Ok(())
}

/// Get the VM label from Lua app data, defaulting to `"lua"`.
fn vm_label(lua: &Lua) -> String {
    lua.app_data_ref::<VmLabel>()
        .map(|l| l.0.clone())
        .unwrap_or_else(|| "lua".into())
}

/// Whether this VM is the init VM (logs at requested level) or a runner VM
/// (logs demoted to debug to avoid N duplicates during pool creation).
fn is_init_vm(lua: &Lua) -> bool {
    lua.app_data_ref::<VmLabel>().is_some_and(|l| l.0 == "init")
}

fn log_info(lua: &Lua, msg: &str) {
    let label = vm_label(lua);

    if is_init_vm(lua) {
        info!("[lua:{label}] {msg}");
    } else {
        debug!("[lua:{label}] {msg}");
    }
}

fn log_warn(lua: &Lua, msg: &str) {
    let label = vm_label(lua);

    warn!("[lua:{label}] {msg}");
}

fn log_error(lua: &Lua, msg: &str) {
    let label = vm_label(lua);

    error!("[lua:{label}] {msg}");
}
