//! `crap.util` and `crap.json` namespaces — slugify, nanoid, JSON encode/decode,
//! date helpers, and pure Lua table/string utilities loaded after the namespace is set.

use anyhow::{Context as _, Result};
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table, Value as LuaValue};
use nanoid::nanoid;
use serde_json::Value;

use super::{json_to_lua, lua_to_json};

/// Pure Lua table and string helpers, compiled in from `lua/util_helpers.lua`.
const LUA_UTIL_HELPERS: &str = include_str!("../../../lua/util_helpers.lua");

/// Register `crap.util` and `crap.json` — slugify, nanoid, JSON, date helpers.
pub(super) fn register_util(lua: &Lua, crap: &Table) -> Result<()> {
    let t = lua.create_table()?;

    t.set(
        "slugify",
        lua.create_function(|_, s: String| Ok(slugify(&s)))?,
    )?;
    t.set("nanoid", lua.create_function(|_, ()| Ok(nanoid!()))?)?;

    let json_encode_fn = lua.create_function(|lua, value: LuaValue| json_encode(lua, &value))?;
    let json_decode_fn = lua.create_function(|lua, s: String| json_decode(lua, &s))?;

    t.set("json_encode", json_encode_fn.clone())?;
    t.set("json_decode", json_decode_fn.clone())?;

    // Dedicated crap.json namespace (aliases)
    let json_table = lua.create_table()?;
    json_table.set("encode", json_encode_fn)?;
    json_table.set("decode", json_decode_fn)?;
    crap.set("json", json_table)?;

    // Date helpers
    t.set("date_now", lua.create_function(|_, ()| date_now())?)?;
    t.set(
        "date_timestamp",
        lua.create_function(|_, ()| date_timestamp())?,
    )?;
    t.set(
        "date_parse",
        lua.create_function(|_, s: String| date_parse(&s))?,
    )?;
    t.set(
        "date_format",
        lua.create_function(|_, (ts, fmt): (i64, String)| date_format(ts, &fmt))?,
    )?;
    t.set(
        "date_add",
        lua.create_function(|_, (ts, secs): (i64, i64)| Ok(ts + secs))?,
    )?;
    t.set(
        "date_diff",
        lua.create_function(|_, (a, b): (i64, i64)| Ok(a - b))?,
    )?;

    crap.set("util", t)?;
    Ok(())
}

/// Load pure Lua helpers onto `crap.util` (must be called after `crap` global is set).
pub(super) fn load_lua_helpers(lua: &Lua) -> Result<()> {
    lua.load(LUA_UTIL_HELPERS)
        .exec()
        .context("Failed to load Lua util helpers")?;
    Ok(())
}

// ── Helper functions ────────────────────────────────────────────────────

/// Encode a Lua value to JSON string.
fn json_encode(lua: &Lua, value: &LuaValue) -> LuaResult<String> {
    let json_value = lua_to_json(lua, value)?;
    serde_json::to_string(&json_value)
        .map_err(|e| RuntimeError(format!("JSON encode error: {e:#}")))
}

/// Decode a JSON string to a Lua value.
fn json_decode(lua: &Lua, s: &str) -> LuaResult<LuaValue> {
    let value: Value =
        serde_json::from_str(s).map_err(|e| RuntimeError(format!("JSON decode error: {e:#}")))?;
    json_to_lua(lua, &value)
}

/// Current time as RFC 3339 string.
fn date_now() -> LuaResult<String> {
    Ok(Utc::now().to_rfc3339())
}

/// Current Unix timestamp.
fn date_timestamp() -> LuaResult<i64> {
    Ok(Utc::now().timestamp())
}

/// Parse a date string into a Unix timestamp. Supports RFC 3339, "YYYY-MM-DD HH:MM:SS", "YYYY-MM-DD".
fn date_parse(s: &str) -> LuaResult<i64> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.timestamp());
    }

    if let Ok(dt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        return Ok(dt.and_utc().timestamp());
    }

    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(d
            .and_hms_opt(0, 0, 0)
            .expect("00:00:00 is valid")
            .and_utc()
            .timestamp());
    }

    Err(RuntimeError(format!("could not parse date: {s}")))
}

/// Format a Unix timestamp with a chrono format string.
fn date_format(ts: i64, fmt: &str) -> LuaResult<String> {
    let dt =
        DateTime::from_timestamp(ts, 0).ok_or_else(|| RuntimeError("invalid timestamp".into()))?;
    Ok(dt.format(fmt).to_string())
}

/// Convert a string to a URL-safe slug.
fn slugify(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Hello World"), "hello-world");
    }

    #[test]
    fn slugify_special_chars() {
        assert_eq!(slugify("Hello, World!"), "hello-world");
    }

    #[test]
    fn slugify_multiple_spaces() {
        assert_eq!(slugify("hello   world"), "hello-world");
    }

    #[test]
    fn slugify_leading_trailing() {
        assert_eq!(slugify("  hello  "), "hello");
    }

    #[test]
    fn slugify_already_clean() {
        assert_eq!(slugify("hello-world"), "hello-world");
    }

    #[test]
    fn slugify_empty() {
        assert_eq!(slugify(""), "");
    }

    #[test]
    fn slugify_unicode() {
        assert_eq!(
            slugify("Caf\u{00e9} Latt\u{00e9}"),
            "caf\u{00e9}-latt\u{00e9}"
        );
    }

    fn setup_lua() -> Lua {
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        register_util(&lua, &crap).unwrap();
        lua.globals().set("crap", crap).unwrap();
        load_lua_helpers(&lua).unwrap();
        lua
    }

    #[test]
    fn json_namespace_encode() {
        let lua = setup_lua();
        let result: String = lua
            .load(r#"return crap.json.encode({ name = "test", count = 42 })"#)
            .eval()
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["name"], "test");
        assert_eq!(parsed["count"], 42);
    }

    #[test]
    fn json_namespace_decode() {
        let lua = setup_lua();
        let result: String = lua
            .load(r#"local t = crap.json.decode('{"hello":"world"}'); return t.hello"#)
            .eval()
            .unwrap();
        assert_eq!(result, "world");
    }

    #[test]
    fn json_namespace_roundtrip() {
        let lua = setup_lua();
        let result: String = lua
            .load(
                r#"
                local original = { items = { "a", "b" }, nested = { x = 1 } }
                local encoded = crap.json.encode(original)
                local decoded = crap.json.decode(encoded)
                return decoded.items[1] .. decoded.items[2] .. tostring(decoded.nested.x)
            "#,
            )
            .eval()
            .unwrap();
        assert_eq!(result, "ab1");
    }

    #[test]
    fn json_util_aliases_still_work() {
        let lua = setup_lua();
        let result: String = lua
            .load(r#"return crap.util.json_encode({ ok = true })"#)
            .eval()
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["ok"], true);
    }
}
