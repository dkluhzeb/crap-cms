//! Register `crap.auth` — `hash_password`, `verify_password`, user.

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

    // crap.auth.default_methods() — returns the standard 3-entry
    // method list (password_login + bearer + session_cookie) for use
    // in `auth = { enabled = true, methods = crap.auth.default_methods() }`.
    auth_table.set(
        "default_methods",
        lua.create_function(|lua, ()| default_methods_table(lua))?,
    )?;

    // crap.auth.with_defaults(extras) — returns `default_methods() ++ extras`.
    // `extras` is a sequence of method tables; appended after the defaults.
    auth_table.set(
        "with_defaults",
        lua.create_function(|lua, extras: Option<Table>| {
            let out = default_methods_table(lua)?;
            if let Some(extras) = extras {
                let start = out.len()?;
                for (i, m) in extras.sequence_values::<Table>().flatten().enumerate() {
                    out.set(start + i64::try_from(i).unwrap_or(i64::MAX) + 1, m)?;
                }
            }
            Ok(out)
        })?,
    )?;

    crap.set("auth", auth_table)?;

    Ok(())
}

/// Build a Lua sequence table containing the three default methods.
/// Kept literal (not synthesized from Rust types) so the Lua side
/// owns the shape — user-facing API is "what you'd write yourself."
fn default_methods_table(lua: &Lua) -> mlua::Result<Table> {
    let methods = lua.create_table()?;

    let password = lua.create_table()?;
    password.set("type", "password_login")?;
    methods.set(1, password)?;

    let bearer = lua.create_table()?;
    bearer.set("type", "bearer")?;
    let bearer_surfaces = lua.create_table()?;
    bearer_surfaces.set(1, "grpc")?;
    bearer_surfaces.set(2, "admin")?;
    bearer.set("surfaces", bearer_surfaces)?;
    methods.set(2, bearer)?;

    let cookie = lua.create_table()?;
    cookie.set("type", "session_cookie")?;
    let cookie_surfaces = lua.create_table()?;
    cookie_surfaces.set(1, "admin")?;
    cookie.set("surfaces", cookie_surfaces)?;
    methods.set(3, cookie)?;

    Ok(methods)
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
