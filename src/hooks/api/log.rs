//! Register `crap.log` — info, warn, error.

use anyhow::Result;
use mlua::{Lua, Table};

use super::VmLabel;

/// Log messages include the VM label (e.g. `[lua:init]`, `[lua:vm-1]`).
pub(super) fn register_log(lua: &Lua, crap: &Table) -> Result<()> {
    let log_table = lua.create_table()?;
    let log_info = lua.create_function(|lua, msg: String| {
        let label = lua.app_data_ref::<VmLabel>().map(|l| l.0.clone()).unwrap_or_else(|| "lua".into());
        tracing::info!("[lua:{label}] {msg}");
        Ok(())
    })?;
    let log_warn = lua.create_function(|lua, msg: String| {
        let label = lua.app_data_ref::<VmLabel>().map(|l| l.0.clone()).unwrap_or_else(|| "lua".into());
        tracing::warn!("[lua:{label}] {msg}");
        Ok(())
    })?;
    let log_error = lua.create_function(|lua, msg: String| {
        let label = lua.app_data_ref::<VmLabel>().map(|l| l.0.clone()).unwrap_or_else(|| "lua".into());
        tracing::error!("[lua:{label}] {msg}");
        Ok(())
    })?;
    log_table.set("info", log_info)?;
    log_table.set("warn", log_warn)?;
    log_table.set("error", log_error)?;
    crap.set("log", log_table)?;
    Ok(())
}
