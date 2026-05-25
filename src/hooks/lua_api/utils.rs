//! `crap.util` and `crap.json` namespaces — slugify, nanoid, JSON encode/decode,
//! date helpers, and pure Lua table/string utilities loaded after the namespace is set.

use anyhow::{Context as _, Result};
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Value as LuaValue};
use nanoid::nanoid;
use serde_json::Value;

use super::{json_to_lua, lua_to_json};
use crate::typegen::lua::{LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table};

/// Pure Lua table and string helpers, compiled in from `util_helpers.lua`.
const LUA_UTIL_HELPERS: &str = include_str!("util_helpers.lua");

// ── crap.util ────────────────────────────────────────────────────────

/// Generate a URL-safe slug from a string.
#[lua_fn(
    path = "crap.util.slugify",
    returns_doc = "Lowercased, hyphenated slug."
)]
fn util_slugify(_: &Lua, #[lua(doc = "Input string.")] str: String) -> LuaResult<String> {
    Ok(slugify(&str))
}

/// Generate a unique nanoid.
#[lua_fn(path = "crap.util.nanoid", returns_doc = "Random nanoid string.")]
fn util_nanoid(_: &Lua) -> LuaResult<String> {
    Ok(nanoid!())
}

/// Current time as RFC 3339 string.
#[lua_fn(path = "crap.util.date_now", returns_doc = "RFC 3339 timestamp.")]
fn util_date_now(_: &Lua) -> LuaResult<String> {
    Ok(Utc::now().to_rfc3339())
}

/// Current Unix timestamp (seconds since epoch).
#[lua_fn(
    path = "crap.util.date_timestamp",
    returns_doc = "Seconds since the Unix epoch."
)]
fn util_date_timestamp(_: &Lua) -> LuaResult<i64> {
    Ok(Utc::now().timestamp())
}

/// Parse a date string into a Unix timestamp. Supports RFC 3339,
/// "YYYY-MM-DD HH:MM:SS", and "YYYY-MM-DD".
#[lua_fn(
    path = "crap.util.date_parse",
    returns_doc = "Seconds since the Unix epoch."
)]
fn util_date_parse(
    _: &Lua,
    #[lua(doc = "Date string (RFC 3339, `YYYY-MM-DD HH:MM:SS`, or `YYYY-MM-DD`).")] s: String,
) -> LuaResult<i64> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(&s) {
        return Ok(dt.timestamp());
    }

    if let Ok(dt) = NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S") {
        return Ok(dt.and_utc().timestamp());
    }

    if let Ok(d) = NaiveDate::parse_from_str(&s, "%Y-%m-%d") {
        return Ok(d
            .and_hms_opt(0, 0, 0)
            .expect("00:00:00 is valid")
            .and_utc()
            .timestamp());
    }

    Err(RuntimeError(format!("could not parse date: {s}")))
}

/// Format a Unix timestamp with a chrono format string.
#[lua_fn(path = "crap.util.date_format", returns_doc = "Formatted date string.")]
fn util_date_format(
    _: &Lua,
    #[lua(doc = "Unix timestamp (seconds).")] ts: i64,
    #[lua(doc = "Chrono format string (e.g. `\"%Y-%m-%d %H:%M:%S\"`).")] fmt: String,
) -> LuaResult<String> {
    let dt =
        DateTime::from_timestamp(ts, 0).ok_or_else(|| RuntimeError("invalid timestamp".into()))?;
    Ok(dt.format(&fmt).to_string())
}

/// Add seconds to a Unix timestamp.
#[lua_fn(
    path = "crap.util.date_add",
    returns_doc = "New timestamp (`ts + secs`)."
)]
fn util_date_add(
    _: &Lua,
    #[lua(doc = "Base timestamp.")] ts: i64,
    #[lua(doc = "Seconds to add (may be negative).")] secs: i64,
) -> LuaResult<i64> {
    Ok(ts + secs)
}

/// Difference (in seconds) between two Unix timestamps (`a - b`).
#[lua_fn(
    path = "crap.util.date_diff",
    returns_doc = "Seconds elapsed (`a - b`)."
)]
fn util_date_diff(
    _: &Lua,
    #[lua(doc = "First timestamp.")] a: i64,
    #[lua(doc = "Second timestamp.")] b: i64,
) -> LuaResult<i64> {
    Ok(a - b)
}

lua_table! {
    name: crap_util,
    path: "crap.util",
    state: (),
    header: "Utility functions.",
    fns: [
        util_slugify,
        util_nanoid,
        util_date_now,
        util_date_timestamp,
        util_date_parse,
        util_date_format,
        util_date_add,
        util_date_diff,
    ],
}

// ── crap.json ────────────────────────────────────────────────────────

/// Encode a Lua value as a JSON string.
#[lua_fn(path = "crap.json.encode", returns_doc = "JSON string.")]
fn json_encode_fn(
    _: &Lua,
    #[lua(ty = "any", doc = "Lua value to encode.")] value: LuaValue,
) -> LuaResult<String> {
    let json_value = lua_to_json(&value)?;
    serde_json::to_string(&json_value)
        .map_err(|e| RuntimeError(format!("JSON encode error: {e:#}")))
}

/// Decode a JSON string into a Lua value.
#[lua_fn(
    path = "crap.json.decode",
    returns = "any",
    returns_doc = "Decoded Lua value."
)]
fn json_decode_fn(lua: &Lua, #[lua(doc = "JSON string.")] str: String) -> LuaResult<LuaValue> {
    let value: Value = serde_json::from_str(&str)
        .map_err(|e| RuntimeError(format!("JSON decode error: {e:#}")))?;
    json_to_lua(lua, &value)
}

lua_table! {
    name: crap_json,
    path: "crap.json",
    state: (),
    header: "JSON encode/decode.",
    fns: [json_encode_fn, json_decode_fn],
}

// ── Registration ─────────────────────────────────────────────────────

/// Register `crap.util` and `crap.json`. Parent `crap` must already be
/// in globals (`register_api` sets it up-front).
pub(super) fn register_util(lua: &Lua) -> Result<()> {
    register_crap_util(lua, ())?;
    register_crap_json(lua, ())?;
    Ok(())
}

/// Load pure Lua helpers onto `crap.util` (must be called after `crap`
/// global is set, and after `register_util` has created `crap.util`).
pub(super) fn load_lua_helpers(lua: &Lua) -> Result<()> {
    lua.load(LUA_UTIL_HELPERS)
        .exec()
        .context("Failed to load Lua util helpers")?;
    Ok(())
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
        lua.globals()
            .set("crap", lua.create_table().unwrap())
            .unwrap();
        register_util(&lua).unwrap();
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
}
