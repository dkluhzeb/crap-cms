//! `crap.util` namespace — slugify, nanoid, JSON encode/decode, date helpers,
//! and pure Lua table/string utilities loaded after the namespace is set.

use anyhow::{Context as _, Result};
use mlua::{Function, Lua, Table};
use serde_json::Value;

/// Pure Lua table and string helpers, loaded onto `crap.util` after the table is set.
const LUA_UTIL_HELPERS: &str = r#"
local util = crap.util

function util.deep_merge(a, b)
    local out = {}
    for k, v in pairs(a) do
        out[k] = v
    end
    for k, v in pairs(b) do

        if type(out[k]) == "table" and type(v) == "table" then
            out[k] = util.deep_merge(out[k], v)
        else
            out[k] = v
        end
    end

    return out
end

function util.pick(tbl, keys)
    local out = {}
    for _, k in ipairs(keys) do
        out[k] = tbl[k]
    end

    return out
end

function util.omit(tbl, keys)
    local skip = {}
    for _, k in ipairs(keys) do skip[k] = true end
    local out = {}
    for k, v in pairs(tbl) do

        if not skip[k] then out[k] = v end
    end

    return out
end

function util.keys(tbl)
    local out = {}
    for k in pairs(tbl) do out[#out + 1] = k end

    return out
end

function util.values(tbl)
    local out = {}
    for _, v in pairs(tbl) do out[#out + 1] = v end

    return out
end

function util.map(tbl, fn)
    local out = {}
    for i, v in ipairs(tbl) do out[i] = fn(v, i) end

    return out
end

function util.filter(tbl, fn)
    local out = {}
    for i, v in ipairs(tbl) do

        if fn(v, i) then out[#out + 1] = v end
    end

    return out
end

function util.find(tbl, fn)
    for i, v in ipairs(tbl) do

        if fn(v, i) then return v end
    end

    return nil
end

function util.includes(tbl, value)
    for _, v in ipairs(tbl) do

        if v == value then return true end
    end

    return false
end

function util.is_empty(tbl)

    return next(tbl) == nil
end

function util.clone(tbl)
    local out = {}
    for k, v in pairs(tbl) do out[k] = v end

    return out
end

function util.trim(str)

    return (str:gsub("^%s+", ""):gsub("%s+$", ""))
end

function util.split(str, sep)
    local out = {}
    local pattern = "([^" .. sep .. "]+)"
    for part in str:gmatch(pattern) do
        out[#out + 1] = part
    end

    return out
end

function util.starts_with(str, prefix)

    return str:sub(1, #prefix) == prefix
end

function util.ends_with(str, suffix)

    return suffix == "" or str:sub(-#suffix) == suffix
end

function util.truncate(str, max_len, suffix)
    suffix = suffix or "..."

    if #str <= max_len then return str end

    return str:sub(1, max_len - #suffix) .. suffix
end
"#;

/// Register `crap.util` — slugify, nanoid, JSON, date helpers.
pub(super) fn register_util(lua: &Lua, crap: &Table) -> Result<()> {
    let util_table = lua.create_table()?;

    let slugify_fn = lua.create_function(|_, s: String| Ok(slugify(&s)))?;
    util_table.set("slugify", slugify_fn)?;

    let nanoid_fn = lua.create_function(|_, ()| Ok(nanoid::nanoid!()))?;
    util_table.set("nanoid", nanoid_fn)?;

    let json_encode_fn: Function = lua.create_function(|lua, value: mlua::Value| {
        let json_value = super::lua_to_json(lua, &value)?;
        serde_json::to_string(&json_value)
            .map_err(|e| mlua::Error::RuntimeError(format!("JSON encode error: {}", e)))
    })?;
    util_table.set("json_encode", json_encode_fn)?;

    let json_decode_fn = lua.create_function(|lua, s: String| {
        let value: Value = serde_json::from_str(&s)
            .map_err(|e| mlua::Error::RuntimeError(format!("JSON decode error: {}", e)))?;
        super::json_to_lua(lua, &value)
    })?;
    util_table.set("json_decode", json_decode_fn)?;

    // Date helpers (Rust, using chrono)
    {
        let date_now_fn = lua.create_function(|_, ()| -> mlua::Result<String> {
            Ok(chrono::Utc::now().to_rfc3339())
        })?;
        util_table.set("date_now", date_now_fn)?;

        let date_timestamp_fn = lua
            .create_function(|_, ()| -> mlua::Result<i64> { Ok(chrono::Utc::now().timestamp()) })?;
        util_table.set("date_timestamp", date_timestamp_fn)?;

        let date_parse_fn = lua.create_function(|_, s: String| -> mlua::Result<i64> {
            // Try RFC 3339 first
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&s) {
                return Ok(dt.timestamp());
            }
            // Try "YYYY-MM-DD HH:MM:SS"
            if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S") {
                return Ok(dt.and_utc().timestamp());
            }
            // Try "YYYY-MM-DD"
            if let Ok(d) = chrono::NaiveDate::parse_from_str(&s, "%Y-%m-%d") {
                return Ok(d
                    .and_hms_opt(0, 0, 0)
                    .expect("00:00:00 is valid")
                    .and_utc()
                    .timestamp());
            }
            Err(mlua::Error::RuntimeError(format!(
                "could not parse date: {}",
                s
            )))
        })?;
        util_table.set("date_parse", date_parse_fn)?;

        let date_format_fn =
            lua.create_function(|_, (ts, fmt): (i64, String)| -> mlua::Result<String> {
                let dt = chrono::DateTime::from_timestamp(ts, 0)
                    .ok_or_else(|| mlua::Error::RuntimeError("invalid timestamp".into()))?;
                Ok(dt.format(&fmt).to_string())
            })?;
        util_table.set("date_format", date_format_fn)?;

        let date_add_fn = lua
            .create_function(|_, (ts, secs): (i64, i64)| -> mlua::Result<i64> { Ok(ts + secs) })?;
        util_table.set("date_add", date_add_fn)?;

        let date_diff_fn =
            lua.create_function(|_, (a, b): (i64, i64)| -> mlua::Result<i64> { Ok(a - b) })?;
        util_table.set("date_diff", date_diff_fn)?;
    }

    crap.set("util", util_table)?;

    Ok(())
}

/// Load pure Lua helpers onto `crap.util` (must be called after `crap` global is set).
pub(super) fn load_lua_helpers(lua: &Lua) -> Result<()> {
    lua.load(LUA_UTIL_HELPERS)
        .exec()
        .context("Failed to load Lua util helpers")?;
    Ok(())
}

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
}
