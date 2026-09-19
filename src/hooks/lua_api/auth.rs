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

    let Some(extras) = extras else {
        return Ok(out);
    };

    let start = out.raw_len();

    for (index, entry) in (1..).zip(extras.sequence_values::<Value>()) {
        let method = require_method_spec(entry?, index)?;
        out.raw_set(start + index, method)?;
    }

    Ok(out)
}

/// An `extras` entry must be an auth method spec — a table with a string
/// `type` — and a malformed one is an error naming its index. It used to be
/// dropped silently, which shipped an auth method list missing an entry the
/// author wrote.
fn require_method_spec(entry: Value, index: usize) -> LuaResult<Table> {
    let method = match entry {
        Value::Table(method) => method,
        other => {
            return Err(RuntimeError(format!(
                "crap.auth.with_defaults: extras[{index}] must be an auth method table, got {}",
                other.type_name()
            )));
        }
    };

    match method.get::<Value>("type")? {
        Value::String(_) => Ok(method),
        Value::Nil => Err(RuntimeError(format!(
            "crap.auth.with_defaults: extras[{index}] is not an auth method spec — it has no \
             string `type` (e.g. \"strategy\")"
        ))),
        other => Err(RuntimeError(format!(
            "crap.auth.with_defaults: extras[{index}].type must be a string, got {}",
            other.type_name()
        ))),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn lua_with_auth() -> Lua {
        let lua = Lua::new();
        lua.globals()
            .set("crap", lua.create_table().unwrap())
            .unwrap();
        register_auth(&lua).unwrap();
        lua
    }

    #[test]
    fn with_defaults_appends_valid_extras_after_the_defaults() {
        let lua = lua_with_auth();
        let methods: Table = lua
            .load(
                r#"return crap.auth.with_defaults({
                    { type = "strategy", name = "api-key", authenticate = "hooks.auth.api_key" },
                })"#,
            )
            .eval()
            .unwrap();

        assert_eq!(methods.raw_len(), 4);
        let last: Table = methods.raw_get(4).unwrap();
        assert_eq!(last.get::<String>("name").unwrap(), "api-key");
    }

    /// A non-table entry used to be `flatten()`-ed away silently; it must
    /// error naming the index.
    #[test]
    fn with_defaults_rejects_a_non_table_entry_naming_the_index() {
        let lua = lua_with_auth();
        let err = lua
            .load(r#"return crap.auth.with_defaults({ { type = "bearer" }, "strategy" })"#)
            .exec()
            .unwrap_err()
            .to_string();
        assert!(err.contains("extras[2]"), "names the index: {err}");
        assert!(err.contains("got string"), "{err}");
    }

    /// A table that is not a method spec (no string `type`) is rejected too.
    #[test]
    fn with_defaults_rejects_a_table_without_a_type() {
        let lua = lua_with_auth();
        let err = lua
            .load(r#"return crap.auth.with_defaults({ { name = "api-key" } })"#)
            .exec()
            .unwrap_err()
            .to_string();
        assert!(err.contains("extras[1]"), "{err}");
        assert!(err.contains("`type`"), "{err}");

        let err = lua
            .load("return crap.auth.with_defaults({ { type = 1 } })")
            .exec()
            .unwrap_err()
            .to_string();
        assert!(err.contains("extras[1].type must be a string"), "{err}");
    }
}
