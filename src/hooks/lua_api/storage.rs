//! Register `crap.storage` — register a custom Lua-delegated upload
//! storage backend for `[upload] storage = "custom"`.
//!
//! ## Usage
//!
//! ```lua
//! crap.storage.register({
//!   put = function(key, data, content_type) ... end,
//!   get = function(key) return data end,
//!   delete = function(key) ... end,
//!   url = function(key) return "https://cdn.example.com/" .. key end,
//! })
//! ```
//!
//! `put`, `get`, and `delete` are required; `url` and `exists` are
//! optional (the backend falls back to `/uploads/<key>` and a `get`
//! probe respectively). The handler functions are stored as
//! `crap._storage` and invoked by the custom storage backend.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table, Value};

use crate::hooks::lifecycle::InitPhase;
use crate::typegen::lua::{LuaFnSpec, LuaParam, lua_fn, lua_table};

/// Allowed keys on a `crap.storage.register` handler table.
const STORAGE_HANDLER_KEYS: &[&str] = &["put", "get", "delete", "url", "exists"];

/// Register a custom storage backend's handler. **Init-only** — call from
/// `init.lua` when `[upload] storage = "custom"`. Stores the handler as
/// `crap._storage`; the custom backend delegates every operation to it.
#[lua_fn(path = "crap.storage.register")]
fn storage_register(
    lua: &Lua,
    #[lua(
        ty = "{ put: fun(key: string, data: string, content_type: string), get: fun(key: string): string?, delete: fun(key: string), url?: fun(key: string): string, exists?: fun(key: string): boolean }",
        doc = "Storage handler. `put`/`get`/`delete` required; `url`/`exists` optional. `get` returns nil for a missing key."
    )]
    handler: Table,
) -> LuaResult<()> {
    if lua.app_data_ref::<InitPhase>().is_none() {
        return Err(RuntimeError(
            "crap.storage.register must be called from init.lua \
             (the custom backend is wired once at startup)"
                .into(),
        ));
    }

    for name in ["put", "get", "delete"] {
        if !matches!(handler.get::<Value>(name)?, Value::Function(_)) {
            return Err(RuntimeError(format!(
                "crap.storage.register: '{name}' must be a function"
            )));
        }
    }

    for name in ["url", "exists"] {
        if !matches!(handler.get::<Value>(name)?, Value::Nil | Value::Function(_)) {
            return Err(RuntimeError(format!(
                "crap.storage.register: '{name}' must be a function when provided"
            )));
        }
    }

    // Reject unknown keys so a typo (`exsits`) surfaces at load instead of
    // silently falling back to the get-probe.
    for pair in handler.clone().pairs::<Value, Value>() {
        let (key, _) = pair?;
        if let Value::String(s) = &key
            && !STORAGE_HANDLER_KEYS.contains(&s.to_str()?.as_ref())
        {
            return Err(RuntimeError(format!(
                "crap.storage.register: unknown key '{}' (allowed: {})",
                s.to_str()?,
                STORAGE_HANDLER_KEYS.join(", ")
            )));
        }
    }

    let crap: Table = lua.globals().get("crap")?;
    crap.set("_storage", handler)?;

    Ok(())
}

lua_table! {
    name: crap_storage,
    path: "crap.storage",
    state: (),
    header: "Register a custom upload-storage backend (for `[upload] storage = \"custom\"`).",
    fns: [storage_register],
}

/// Register `crap.storage`. Parent `crap` table must already be in globals.
pub(super) fn register_storage(lua: &Lua) -> Result<()> {
    register_crap_storage(lua, ())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lua_in_init_phase() -> Lua {
        let lua = Lua::new();
        lua.globals()
            .set("crap", lua.create_table().unwrap())
            .unwrap();
        register_storage(&lua).unwrap();
        lua.set_app_data(InitPhase);
        lua
    }

    #[test]
    fn register_sets_crap_storage() {
        let lua = lua_in_init_phase();
        lua.load(
            r"
            crap.storage.register({
              put = function(k, d, c) end,
              get = function(k) return '' end,
              delete = function(k) end,
              url = function(k) return k end,
            })
            ",
        )
        .exec()
        .unwrap();

        let crap: Table = lua.globals().get("crap").unwrap();
        let storage: Table = crap.get("_storage").unwrap();
        assert!(matches!(
            storage.get::<Value>("put").unwrap(),
            Value::Function(_)
        ));
    }

    #[test]
    fn missing_required_function_is_rejected() {
        let lua = lua_in_init_phase();
        let err = lua
            .load(r"crap.storage.register({ get = function(k) end })")
            .exec()
            .unwrap_err()
            .to_string();
        assert!(err.contains("put"), "should name the missing fn: {err}");
    }

    #[test]
    fn non_function_optional_is_rejected() {
        let lua = lua_in_init_phase();
        let err = lua
            .load(
                r"crap.storage.register({
                  put = function() end, get = function() end, delete = function() end,
                  url = 'not a function',
                })",
            )
            .exec()
            .unwrap_err()
            .to_string();
        assert!(err.contains("url"), "should name the bad key: {err}");
    }

    /// Regression: unknown handler keys (a typo like `exsits`) were silently
    /// accepted, so the operator's existence check never ran.
    #[test]
    fn unknown_handler_key_is_rejected() {
        let lua = lua_in_init_phase();
        let err = lua
            .load(
                r"crap.storage.register({
                  put = function() end, get = function() end, delete = function() end,
                  exsits = function(k) return true end,
                })",
            )
            .exec()
            .unwrap_err()
            .to_string();
        assert!(err.contains("exsits"), "should name the unknown key: {err}");
    }

    #[test]
    fn register_outside_init_phase_is_rejected() {
        let lua = Lua::new();
        lua.globals()
            .set("crap", lua.create_table().unwrap())
            .unwrap();
        register_storage(&lua).unwrap();
        // No InitPhase — simulate a runtime call.

        let err = lua
            .load(
                r"crap.storage.register({
                  put = function() end, get = function() end, delete = function() end,
                })",
            )
            .exec()
            .unwrap_err()
            .to_string();
        assert!(err.contains("init.lua"), "expected init-only error: {err}");
    }
}
