//! Register `crap.auth` — hash_password, verify_password, user.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table, Value};

use crate::{
    core::auth::{hash_password, verify_password},
    hooks::lifecycle::{UserContext, converters::document_to_lua_table},
};

/// Register `crap.auth.hash_password` and `crap.auth.verify_password` Lua functions.
pub(super) fn register_auth(lua: &Lua, crap: &Table) -> Result<()> {
    let auth_table = lua.create_table()?;

    auth_table.set(
        "hash_password",
        lua.create_function(|_, password: String| hash(&password))?,
    )?;

    auth_table.set(
        "verify_password",
        lua.create_function(|_, (password, h): (String, String)| verify(&password, &h))?,
    )?;

    auth_table.set("user", lua.create_function(user)?)?;

    crap.set("auth", auth_table)?;

    Ok(())
}

/// Return the current hook user document, or nil if no user is set.
fn user(lua: &Lua, _: ()) -> mlua::Result<Value> {
    let user = lua
        .app_data_ref::<UserContext>()
        .and_then(|ctx| ctx.0.clone());

    match user {
        Some(doc) => Ok(Value::Table(document_to_lua_table(lua, &doc)?)),
        None => Ok(Value::Nil),
    }
}

/// Hash a plaintext password, returning the Argon2 hash string.
fn hash(password: &str) -> mlua::Result<String> {
    hash_password(password)
        .map(|h| h.as_ref().to_string())
        .map_err(|e| RuntimeError(format!("hash_password error: {e:#}")))
}

/// Verify a password against a hash.
fn verify(password: &str, hash: &str) -> mlua::Result<bool> {
    verify_password(password, hash)
        .map_err(|e| RuntimeError(format!("verify_password error: {e:#}")))
}
