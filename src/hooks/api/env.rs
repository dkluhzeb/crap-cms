//! Register `crap.env` — read-only env var access.

use anyhow::Result;
use mlua::{Lua, Result as LuaResult, Table};
use std::env;

/// Register `crap.env.get` — read-only, restricted to `CRAP_*` and `LUA_*` prefixed vars.
pub(super) fn register_env(lua: &Lua, crap: &Table) -> Result<()> {
    let t = lua.create_table()?;
    t.set("get", lua.create_function(|_, key: String| env_get(&key))?)?;
    crap.set("env", t)?;

    Ok(())
}

/// Read an environment variable, restricted to `CRAP_*` and `LUA_*` prefixes.
fn env_get(key: &str) -> LuaResult<Option<String>> {
    if !key.starts_with("CRAP_") && !key.starts_with("LUA_") {
        return Ok(None);
    }

    Ok(env::var(key).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_lua() -> (Lua, mlua::Result<Table>) {
        let lua = Lua::new();
        let result = lua.create_table();
        (lua, result)
    }

    #[test]
    fn allows_crap_prefixed_var() {
        // SAFETY: Test is single-threaded; no concurrent env access.
        unsafe { std::env::set_var("CRAP_TEST_VAR", "hello") };
        let (lua, crap) = setup_lua();
        let crap = crap.unwrap();
        register_env(&lua, &crap).unwrap();
        let env: Table = crap.get("env").unwrap();
        let get: mlua::Function = env.get("get").unwrap();
        let val: Option<String> = get.call("CRAP_TEST_VAR").unwrap();
        assert_eq!(val, Some("hello".to_string()));
        // SAFETY: Test is single-threaded; no concurrent env access.
        unsafe { std::env::remove_var("CRAP_TEST_VAR") };
    }

    #[test]
    fn allows_lua_prefixed_var() {
        // SAFETY: Test is single-threaded; no concurrent env access.
        unsafe { std::env::set_var("LUA_TEST_VAR", "world") };
        let (lua, crap) = setup_lua();
        let crap = crap.unwrap();
        register_env(&lua, &crap).unwrap();
        let env: Table = crap.get("env").unwrap();
        let get: mlua::Function = env.get("get").unwrap();
        let val: Option<String> = get.call("LUA_TEST_VAR").unwrap();
        assert_eq!(val, Some("world".to_string()));
        // SAFETY: Test is single-threaded; no concurrent env access.
        unsafe { std::env::remove_var("LUA_TEST_VAR") };
    }

    #[test]
    fn blocks_unprefixed_var() {
        // SAFETY: Test is single-threaded; no concurrent env access.
        unsafe { std::env::set_var("HOME_TEST", "/tmp") };
        let (lua, crap) = setup_lua();
        let crap = crap.unwrap();
        register_env(&lua, &crap).unwrap();
        let env: Table = crap.get("env").unwrap();
        let get: mlua::Function = env.get("get").unwrap();
        let val: Option<String> = get.call("HOME_TEST").unwrap();
        assert_eq!(val, None);
        // SAFETY: Test is single-threaded; no concurrent env access.
        unsafe { std::env::remove_var("HOME_TEST") };
    }

    #[test]
    fn blocks_sensitive_vars() {
        let (lua, crap) = setup_lua();
        let crap = crap.unwrap();
        register_env(&lua, &crap).unwrap();
        let env: Table = crap.get("env").unwrap();
        let get: mlua::Function = env.get("get").unwrap();

        // These should all return None regardless of whether they exist
        for key in [
            "PATH",
            "HOME",
            "SECRET_KEY",
            "DATABASE_URL",
            "AWS_SECRET_ACCESS_KEY",
        ] {
            let val: Option<String> = get.call(key).unwrap();
            assert_eq!(val, None, "Expected None for blocked key: {}", key);
        }
    }

    #[test]
    fn returns_none_for_nonexistent_allowed_var() {
        let (lua, crap) = setup_lua();
        let crap = crap.unwrap();
        register_env(&lua, &crap).unwrap();
        let env: Table = crap.get("env").unwrap();
        let get: mlua::Function = env.get("get").unwrap();
        let val: Option<String> = get.call("CRAP_NONEXISTENT_VAR_12345").unwrap();
        assert_eq!(val, None);
    }
}
