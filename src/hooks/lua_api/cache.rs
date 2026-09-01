//! Register `crap.cache` — register a custom Lua-delegated cross-request
//! cache backend for `[cache] backend = "custom"`.
//!
//! ## Usage
//!
//! ```lua
//! crap.cache.register({
//!   get = function(key) return value end,   -- string, or nil on miss
//!   set = function(key, value) ... end,
//!   delete = function(key) ... end,
//!   clear = function() ... end,
//!   has = function(key) return true end,    -- optional (falls back to get)
//! })
//! ```
//!
//! `get`, `set`, `delete`, and `clear` are required; `has` is optional (the
//! backend falls back to a `get` probe). The handler functions are stored as
//! `crap._cache` and invoked by the custom cache backend
//! ([`crate::core::cache::CustomCache`]). Values are opaque binary strings —
//! store them byte-exact.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table, Value};

use super::utils::require_init_phase;
use crate::typegen::lua::{LuaFnSpec, LuaParam, lua_fn, lua_table};

/// Allowed keys on a `crap.cache.register` handler table.
const CACHE_HANDLER_KEYS: &[&str] = &["get", "set", "delete", "clear", "has"];

/// Register a custom cache backend's handler. **Init-only** — call from
/// `init.lua` when `[cache] backend = "custom"`. Stores the handler as
/// `crap._cache`; the custom backend delegates every operation to it.
#[lua_fn(path = "crap.cache.register")]
fn cache_register(
    lua: &Lua,
    #[lua(
        ty = "{ get: fun(key: string): string?, set: fun(key: string, value: string), delete: fun(key: string), clear: fun(), has?: fun(key: string): boolean }",
        doc = "Cache handler. `get`/`set`/`delete`/`clear` required; `has` optional. `get` returns nil for a miss; values are opaque binary strings."
    )]
    handler: Table,
) -> LuaResult<()> {
    require_init_phase(
        lua,
        "crap.cache.register must be called from init.lua \
         (the custom backend is wired once at startup)",
    )?;

    for name in ["get", "set", "delete", "clear"] {
        if !matches!(handler.get::<Value>(name)?, Value::Function(_)) {
            return Err(RuntimeError(format!(
                "crap.cache.register: '{name}' must be a function"
            )));
        }
    }

    if !matches!(
        handler.get::<Value>("has")?,
        Value::Nil | Value::Function(_)
    ) {
        return Err(RuntimeError(
            "crap.cache.register: 'has' must be a function when provided".into(),
        ));
    }

    // Reject unknown keys so a typo (`cleer`) surfaces at load instead of
    // silently leaving the real operation on its fallback.
    for pair in handler.clone().pairs::<Value, Value>() {
        let (key, _) = pair?;
        if let Value::String(s) = &key
            && !CACHE_HANDLER_KEYS.contains(&s.to_str()?.as_ref())
        {
            return Err(RuntimeError(format!(
                "crap.cache.register: unknown key '{}' (allowed: {})",
                s.to_str()?,
                CACHE_HANDLER_KEYS.join(", ")
            )));
        }
    }

    let crap: Table = lua.globals().get("crap")?;
    crap.set("_cache", handler)?;

    Ok(())
}

lua_table! {
    name: crap_cache,
    path: "crap.cache",
    state: (),
    header: "Register a custom cross-request cache backend (for `[cache] backend = \"custom\"`).",
    fns: [cache_register],
}

/// Register `crap.cache`. Parent `crap` table must already be in globals.
pub(super) fn register_cache(lua: &Lua) -> Result<()> {
    register_crap_cache(lua, ())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::lifecycle::InitPhase;

    fn lua_in_init_phase() -> Lua {
        let lua = Lua::new();
        lua.globals()
            .set("crap", lua.create_table().unwrap())
            .unwrap();
        register_cache(&lua).unwrap();
        lua.set_app_data(InitPhase);
        lua
    }

    const FULL_HANDLER: &str = r"
        crap.cache.register({
          get = function(k) return nil end,
          set = function(k, v) end,
          delete = function(k) end,
          clear = function() end,
        })
    ";

    #[test]
    fn register_sets_crap_cache() {
        let lua = lua_in_init_phase();
        lua.load(FULL_HANDLER).exec().unwrap();

        let crap: Table = lua.globals().get("crap").unwrap();
        let cache: Table = crap.get("_cache").unwrap();
        assert!(matches!(
            cache.get::<Value>("get").unwrap(),
            Value::Function(_)
        ));
    }

    #[test]
    fn missing_required_function_is_rejected() {
        let lua = lua_in_init_phase();
        let err = lua
            .load(r"crap.cache.register({ get = function(k) end })")
            .exec()
            .unwrap_err()
            .to_string();
        assert!(err.contains("'set'"), "should name the missing fn: {err}");
    }

    #[test]
    fn non_function_optional_has_is_rejected() {
        let lua = lua_in_init_phase();
        let err = lua
            .load(
                r"crap.cache.register({
                    get = function(k) end, set = function(k, v) end,
                    delete = function(k) end, clear = function() end,
                    has = true,
                })",
            )
            .exec()
            .unwrap_err()
            .to_string();
        assert!(err.contains("'has' must be a function"), "{err}");
    }

    #[test]
    fn unknown_key_is_rejected() {
        let lua = lua_in_init_phase();
        let err = lua
            .load(
                r"crap.cache.register({
                    get = function(k) end, set = function(k, v) end,
                    delete = function(k) end, clear = function() end,
                    cleanup = function() end,
                })",
            )
            .exec()
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown key 'cleanup'"), "{err}");
    }

    #[test]
    fn register_outside_init_phase_errors() {
        let lua = lua_in_init_phase();
        lua.remove_app_data::<InitPhase>();
        let err = lua.load(FULL_HANDLER).exec().unwrap_err().to_string();
        assert!(err.contains("init.lua"), "{err}");
    }
}
