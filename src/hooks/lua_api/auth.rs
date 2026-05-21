//! Register `crap.auth` — `hash_password`, `verify_password`, user.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table, Value};

use crate::core::auth::{hash_password, verify_password};
use crate::hooks::lifecycle::{UserContext, converters::document_to_lua_table};
use crate::typegen::lua::{LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table};

/// Hash a plaintext password (Argon2id).
#[lua_fn(path = "crap.auth.hash_password", returns_doc = "Hashed password.")]
fn hash_password_fn(
    _: &Lua,
    #[lua(doc = "Plaintext password.")] password: String,
) -> LuaResult<String> {
    hash_password(&password)
        .map(|h| h.as_ref().to_string())
        .map_err(|e| RuntimeError(format!("hash_password error: {e:#}")))
}

/// Verify a password against a hash.
#[lua_fn(path = "crap.auth.verify_password")]
fn verify_password_fn(
    _: &Lua,
    #[lua(doc = "Plaintext password.")] password: String,
    #[lua(doc = "Stored hash.")] hash: String,
) -> LuaResult<bool> {
    verify_password(&password, &hash)
        .map_err(|e| RuntimeError(format!("verify_password error: {e:#}")))
}

/// Return the currently authenticated user document for the in-flight request, or nil.
/// Returns nil from init.lua, on unauthenticated requests, or outside a hook context.
#[lua_fn(path = "crap.auth.user", returns = "crap.Document?")]
fn user_fn(lua: &Lua) -> LuaResult<Value> {
    let user = lua
        .app_data_ref::<UserContext>()
        .and_then(|ctx| ctx.0.clone());

    match user {
        Some(doc) => Ok(Value::Table(document_to_lua_table(lua, &doc)?)),
        None => Ok(Value::Nil),
    }
}

/// The standard 3-method auth set: `password_login` + `bearer` (all
/// surfaces) + `session_cookie` (admin only). Use in collection
/// definitions:
///
/// ```lua
/// auth = {
///   enabled = true,
///   methods = crap.auth.default_methods(),
/// }
/// ```
#[lua_fn(path = "crap.auth.default_methods", returns = "crap.AuthMethod[]")]
fn default_methods_fn(lua: &Lua) -> LuaResult<Table> {
    default_methods_table(lua)
}

/// Returns `default_methods()` with `extras` appended. The most
/// common shape for "I want the standard auth plus my own strategy":
///
/// ```lua
/// auth = {
///   enabled = true,
///   methods = crap.auth.with_defaults({
///     { type = "strategy", name = "api-key",
///       authenticate = "hooks.auth.api_key",
///       activates_on = { header = "x-api-key" },
///       surfaces = { "grpc" } },
///   }),
/// }
/// ```
#[lua_fn(path = "crap.auth.with_defaults", returns = "crap.AuthMethod[]")]
fn with_defaults_fn(
    lua: &Lua,
    #[lua(
        ty = "crap.AuthMethod[]",
        doc = "Methods to append after the defaults."
    )]
    extras: Option<Table>,
) -> LuaResult<Table> {
    let out = default_methods_table(lua)?;
    if let Some(extras) = extras {
        let start = out.len()?;
        for (i, m) in extras.sequence_values::<Table>().flatten().enumerate() {
            out.set(start + i64::try_from(i).unwrap_or(i64::MAX) + 1, m)?;
        }
    }
    Ok(out)
}

lua_table! {
    name: crap_auth,
    path: "crap.auth",
    state: (),
    header: "Password hashing and verification helpers.",
    fns: [hash_password_fn, verify_password_fn, user_fn, default_methods_fn, with_defaults_fn],
}

/// Register `crap.auth` on the parent table. Parent must already be in
/// globals (`register_api` sets it up-front).
pub(super) fn register_auth(lua: &Lua) -> Result<()> {
    register_crap_auth(lua, ())?;
    Ok(())
}

/// Build a Lua sequence table containing the three default methods.
/// Kept literal (not synthesized from Rust types) so the Lua side
/// owns the shape — user-facing API is "what you'd write yourself."
fn default_methods_table(lua: &Lua) -> LuaResult<Table> {
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
