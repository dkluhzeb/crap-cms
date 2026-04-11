//! Register `crap.log` — info, warn, error.

use anyhow::Result;
use mlua::{Lua, Table};
use tracing::{error, info, warn};

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

fn log_info(lua: &Lua, msg: &str) {
    let label = vm_label(lua);

    info!("[lua:{label}] {msg}");
}

fn log_warn(lua: &Lua, msg: &str) {
    let label = vm_label(lua);

    warn!("[lua:{label}] {msg}");
}

fn log_error(lua: &Lua, msg: &str) {
    let label = vm_label(lua);

    error!("[lua:{label}] {msg}");
}
