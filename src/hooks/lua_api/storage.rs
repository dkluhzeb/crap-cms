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
//!   exists = function(key) return true end,
//!   stat = function(key) return { size = 1234, etag = "v1" } end,
//!   get_range = function(key, first, last) return slice end,
//! })
//! ```
//!
//! `put`, `get`, and `delete` are required; `exists` is optional (the
//! backend falls back to `stat`, or else a `get` probe). `stat` and `get_range` are optional
//! and come as a pair: with them the serve route answers conditional
//! requests from `stat` and streams a body in ranged reads instead of
//! reading the whole object through `get`. The served URL is always the
//! backend-agnostic `/uploads/<key>` proxy path (a frozen pin), so no
//! `url` handler exists. The handler functions are stored as
//! `crap._storage` and invoked by the custom storage backend.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table, Value};

use super::utils::require_init_phase;
use crate::typegen::lua::{LuaFnSpec, LuaParam, lua_fn, lua_table};

/// Allowed keys on a `crap.storage.register` handler table.
const STORAGE_HANDLER_KEYS: &[&str] = &["put", "get", "delete", "exists", "stat", "get_range"];

/// Optional handlers that must be registered together or not at all.
const RANGED_HANDLER_KEYS: [&str; 2] = ["stat", "get_range"];

/// Register a custom storage backend's handler. **Init-only** — call from
/// `init.lua` when `[upload] storage = "custom"`. Stores the handler as
/// `crap._storage`; the custom backend delegates every operation to it.
#[lua_fn(path = "crap.storage.register")]
fn storage_register(
    lua: &Lua,
    #[lua(
        ty = "{ put: fun(key: string, data: string, content_type: string), get: (fun(key: string): string?), delete: fun(key: string), exists?: (fun(key: string): boolean), stat?: (fun(key: string): crap.StorageStat?), get_range?: (fun(key: string, first: integer, last: integer): string?) }",
        doc = "Storage handler. `put`/`get`/`delete` required; `exists` optional. `get` returns nil for a missing key. `stat` and `get_range` are optional and come together: `stat` returns the object's metadata (nil when missing), `get_range` returns exactly bytes `first`..`last` (0-based, inclusive; nil when missing) — the serve route then streams bodies in ranged reads."
    )]
    handler: Table,
) -> LuaResult<()> {
    require_init_phase(
        lua,
        "crap.storage.register must be called from init.lua \
         (the custom backend is wired once at startup)",
    )?;

    for name in ["put", "get", "delete"] {
        if !matches!(handler.get::<Value>(name)?, Value::Function(_)) {
            return Err(RuntimeError(format!(
                "crap.storage.register: '{name}' must be a function"
            )));
        }
    }

    for name in ["exists", "stat", "get_range"] {
        if !matches!(handler.get::<Value>(name)?, Value::Nil | Value::Function(_)) {
            return Err(RuntimeError(format!(
                "crap.storage.register: '{name}' must be a function when provided"
            )));
        }
    }

    check_ranged_pair(&handler)?;

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

/// `stat` and `get_range` only work as a pair: `stat` sizes the object the
/// ranged reads are cut from.
fn check_ranged_pair(handler: &Table) -> LuaResult<()> {
    let [stat, get_range] = RANGED_HANDLER_KEYS.map(|name| handler.contains_key(name));

    if stat? == get_range? {
        return Ok(());
    }

    Err(RuntimeError(
        "crap.storage.register: 'stat' and 'get_range' must be provided together".to_string(),
    ))
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
    use crate::hooks::lifecycle::InitPhase;

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
    fn ranged_handlers_are_accepted_as_a_pair() {
        let lua = lua_in_init_phase();
        lua.load(
            r"crap.storage.register({
              put = function() end, get = function() end, delete = function() end,
              stat = function(k) return { size = 0 } end,
              get_range = function(k, first, last) return '' end,
            })",
        )
        .exec()
        .unwrap();

        let crap: Table = lua.globals().get("crap").unwrap();
        let storage: Table = crap.get("_storage").unwrap();
        assert!(matches!(
            storage.get::<Value>("get_range").unwrap(),
            Value::Function(_)
        ));
    }

    /// `stat` without `get_range` (or the reverse) cannot stream — refused
    /// at registration rather than silently falling back to whole reads.
    #[test]
    fn a_lone_ranged_handler_is_rejected() {
        for lone in ["stat", "get_range"] {
            let lua = lua_in_init_phase();
            let err = lua
                .load(format!(
                    "crap.storage.register({{
                      put = function() end, get = function() end, delete = function() end,
                      {lone} = function() end,
                    }})"
                ))
                .exec()
                .unwrap_err()
                .to_string();

            assert!(err.contains("must be provided together"), "{lone}: {err}");
        }
    }

    #[test]
    fn a_non_function_ranged_handler_is_rejected() {
        let lua = lua_in_init_phase();
        let err = lua
            .load(
                r"crap.storage.register({
                  put = function() end, get = function() end, delete = function() end,
                  stat = 'nope', get_range = function() end,
                })",
            )
            .exec()
            .unwrap_err()
            .to_string();

        assert!(err.contains("'stat' must be a function"), "{err}");
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
