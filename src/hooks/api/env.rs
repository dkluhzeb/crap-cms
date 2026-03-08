//! Register `crap.env` — read-only env var access.

use anyhow::Result;
use mlua::{Lua, Table};

pub(super) fn register_env(lua: &Lua, crap: &Table) -> Result<()> {
    let env_table = lua.create_table()?;
    let env_get_fn = lua.create_function(|_, key: String| -> mlua::Result<Option<String>> {
        match std::env::var(&key) {
            Ok(val) => Ok(Some(val)),
            Err(_) => Ok(None),
        }
    })?;
    env_table.set("get", env_get_fn)?;
    crap.set("env", env_table)?;
    Ok(())
}
