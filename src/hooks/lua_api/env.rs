//! Register `crap.env` — read-only env var access.

use anyhow::Result;
use mlua::{Lua, Result as LuaResult};
use std::env;

use crate::typegen::lua::{LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table};

/// Get an environment variable.
#[lua_fn(path = "crap.env.get", returns_doc = "Value or nil if not set.")]
fn env_get(_: &Lua, #[lua(doc = "Variable name.")] key: String) -> LuaResult<Option<String>> {
    if !key.starts_with("CRAP_") && !key.starts_with("LUA_") {
        return Ok(None);
    }
    Ok(env::var(&key).ok())
}

lua_table! {
    name: crap_env,
    path: "crap.env",
    state: (),
    header: "Read-only access to environment variables.",
    fns: [env_get],
}

/// Register `crap.env` — read-only, restricted to `CRAP_*` and `LUA_*`
/// prefixed vars. The parent `crap` table must already be in globals
/// (`register_api` sets it up-front).
pub(super) fn register_env(lua: &Lua) -> Result<()> {
    register_crap_env(lua, ())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::{Function, Table};

    fn setup_lua() -> Lua {
        let lua = Lua::new();
        lua.globals()
            .set("crap", lua.create_table().unwrap())
            .unwrap();
        register_env(&lua).unwrap();
        lua
    }

    fn env_get_fn(lua: &Lua) -> Function {
        let crap: Table = lua.globals().get("crap").unwrap();
        let env: Table = crap.get("env").unwrap();
        env.get("get").unwrap()
    }

    #[test]
    fn allows_crap_prefixed_var() {
        // SAFETY: Test is single-threaded; no concurrent env access.
        unsafe { std::env::set_var("CRAP_TEST_VAR", "hello") };
        let lua = setup_lua();
        let val: Option<String> = env_get_fn(&lua).call("CRAP_TEST_VAR").unwrap();
        assert_eq!(val, Some("hello".to_string()));
        // SAFETY: Test is single-threaded; no concurrent env access.
        unsafe { std::env::remove_var("CRAP_TEST_VAR") };
    }

    #[test]
    fn allows_lua_prefixed_var() {
        // SAFETY: Test is single-threaded; no concurrent env access.
        unsafe { std::env::set_var("LUA_TEST_VAR", "world") };
        let lua = setup_lua();
        let val: Option<String> = env_get_fn(&lua).call("LUA_TEST_VAR").unwrap();
        assert_eq!(val, Some("world".to_string()));
        // SAFETY: Test is single-threaded; no concurrent env access.
        unsafe { std::env::remove_var("LUA_TEST_VAR") };
    }

    #[test]
    fn blocks_unprefixed_var() {
        // SAFETY: Test is single-threaded; no concurrent env access.
        unsafe { std::env::set_var("HOME_TEST", "/tmp") };
        let lua = setup_lua();
        let val: Option<String> = env_get_fn(&lua).call("HOME_TEST").unwrap();
        assert_eq!(val, None);
        // SAFETY: Test is single-threaded; no concurrent env access.
        unsafe { std::env::remove_var("HOME_TEST") };
    }

    #[test]
    fn blocks_sensitive_vars() {
        let lua = setup_lua();
        let get = env_get_fn(&lua);

        // These should all return None regardless of whether they exist.
        for key in [
            "PATH",
            "HOME",
            "SECRET_KEY",
            "DATABASE_URL",
            "AWS_SECRET_ACCESS_KEY",
        ] {
            let val: Option<String> = get.call(key).unwrap();
            assert_eq!(val, None, "Expected None for blocked key: {key}");
        }
    }

    #[test]
    fn returns_none_for_nonexistent_allowed_var() {
        let lua = setup_lua();
        let val: Option<String> = env_get_fn(&lua).call("CRAP_NONEXISTENT_VAR_12345").unwrap();
        assert_eq!(val, None);
    }
}
