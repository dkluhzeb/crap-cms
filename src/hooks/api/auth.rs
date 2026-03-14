//! Register `crap.auth` — hash_password, verify_password.

use anyhow::Result;
use mlua::{Lua, Table};

use crate::core::auth::{hash_password, verify_password};

pub(super) fn register_auth(lua: &Lua, crap: &Table) -> Result<()> {
    let auth_table = lua.create_table()?;
    let hash_pw_fn = lua.create_function(|_, password: String| {
        hash_password(&password)
            .map(|h| h.as_ref().to_string())
            .map_err(|e| mlua::Error::RuntimeError(format!("hash_password error: {}", e)))
    })?;
    let verify_pw_fn = lua.create_function(|_, (password, hash): (String, String)| {
        verify_password(&password, &hash)
            .map_err(|e| mlua::Error::RuntimeError(format!("verify_password error: {}", e)))
    })?;
    auth_table.set("hash_password", hash_pw_fn)?;
    auth_table.set("verify_password", verify_pw_fn)?;
    crap.set("auth", auth_table)?;
    Ok(())
}
