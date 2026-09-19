//! `crap.util` and `crap.json` namespaces — slugify, nanoid, JSON encode/decode,
//! date helpers, and pure Lua table/string utilities loaded after the namespace is set.

use anyhow::{Context as _, Result};
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc, format::StrftimeItems};
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Value as LuaValue};
use nanoid::nanoid;
use serde_json::Value;

use super::{integer::LuaInt, json_to_lua, lua_to_json};
use crate::hooks::lifecycle::InitPhase;
use crate::typegen::lua::{LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table};

/// Pure Lua table and string helpers, compiled in from `util_helpers.lua`.
const LUA_UTIL_HELPERS: &str = include_str!("util_helpers.lua");

/// Message prefix for a poisoned registry lock — one string so every Lua
/// registration site reports it identically (and greppably).
pub(crate) const REGISTRY_LOCK_POISONED: &str = "Registry lock poisoned";

/// Guard a `crap.*.define` / `.register` entry point to init-phase only.
///
/// Returns `Err(msg)` when the VM has no [`InitPhase`] app-data (a runtime hook
/// VM), so a post-boot registry mutation — which has no effect on the running
/// server — is rejected instead of silently ignored. The single source of the
/// init-only boundary check, shared by every registration surface; each caller
/// passes its own tailored message.
pub(crate) fn require_init_phase(lua: &Lua, msg: &str) -> LuaResult<()> {
    if lua.app_data_ref::<InitPhase>().is_none() {
        return Err(RuntimeError(msg.to_string()));
    }

    Ok(())
}

/// Map a poisoned-lock error to an mlua `RuntimeError` carrying the shared
/// [`REGISTRY_LOCK_POISONED`] message and the cause chain — so the registry
/// write sites don't each hand-roll `format!("Registry lock poisoned: {e:#}")`.
pub(crate) fn registry_lock_poisoned<E: std::fmt::Display>(e: E) -> mlua::Error {
    RuntimeError(format!("{REGISTRY_LOCK_POISONED}: {e:#}"))
}

/// Convert any displayable error into an mlua `RuntimeError`, rendering the full
/// cause chain (`{:#}`).
///
/// The single conversion for the ~50 `crap.*` sites that hand-rolled
/// `RuntimeError(e.to_string())` / `format!("{e}")` / `format!("{e:#}")` with
/// inconsistent verbosity. Standardizing on `{:#}` is a superset — it never
/// drops a message, only appends the anyhow cause chain where one exists — so a
/// bare error → runtime error now reads the same across the whole Lua surface.
/// Use as `.map_err(lua_err)`.
pub(crate) fn lua_err<E: std::fmt::Display>(e: E) -> mlua::Error {
    RuntimeError(format!("{e:#}"))
}

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
    #[lua(ty = "integer", doc = "Unix timestamp (seconds).")] ts: LuaInt,
    #[lua(doc = "Chrono format string (e.g. `\"%Y-%m-%d %H:%M:%S\"`).")] fmt: String,
) -> LuaResult<String> {
    let dt = DateTime::from_timestamp(ts.0, 0)
        .ok_or_else(|| RuntimeError("invalid timestamp".into()))?;

    // Pre-validate the format string: chrono's `DelayedFormat` Display panics
    // (via `.to_string()`) on an unknown specifier, so a Lua caller must not be
    // able to reach `.format().to_string()` with a bad pattern.
    if StrftimeItems::new(&fmt).any(|item| matches!(item, chrono::format::Item::Error)) {
        return Err(RuntimeError(format!("invalid date format string: {fmt}")));
    }

    Ok(dt.format(&fmt).to_string())
}

/// Add seconds to a Unix timestamp.
#[lua_fn(
    path = "crap.util.date_add",
    returns_doc = "New timestamp (`ts + secs`)."
)]
fn util_date_add(
    _: &Lua,
    #[lua(ty = "integer", doc = "Base timestamp.")] ts: LuaInt,
    #[lua(ty = "integer", doc = "Seconds to add (may be negative).")] secs: LuaInt,
) -> LuaResult<i64> {
    ts.0.checked_add(secs.0)
        .ok_or_else(|| RuntimeError("date_add overflow".into()))
}

/// Difference (in seconds) between two Unix timestamps (`a - b`).
#[lua_fn(
    path = "crap.util.date_diff",
    returns_doc = "Seconds elapsed (`a - b`)."
)]
fn util_date_diff(
    _: &Lua,
    #[lua(ty = "integer", doc = "First timestamp.")] a: LuaInt,
    #[lua(ty = "integer", doc = "Second timestamp.")] b: LuaInt,
) -> LuaResult<i64> {
    a.0.checked_sub(b.0)
        .ok_or_else(|| RuntimeError("date_diff overflow".into()))
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
    use crate::hooks::lifecycle::InitPhase;

    /// The shared error converter renders the FULL anyhow cause chain (`{:#}`),
    /// so a bare error → `RuntimeError` keeps its causes uniformly across the
    /// Lua surface (some sites previously dropped them via `to_string()`).
    #[test]
    fn lua_err_renders_the_cause_chain() {
        let err = anyhow::anyhow!("root cause").context("outer context");
        let lua_error = lua_err(err);
        let msg = lua_error.to_string();

        assert!(msg.contains("outer context"), "outer: {msg}");
        assert!(msg.contains("root cause"), "cause chain dropped: {msg}");
    }

    /// The shared init-phase guard: rejected on a runtime VM (no `InitPhase`
    /// app-data), allowed once the marker is set. The one check behind every
    /// `crap.*.define` / `.register` init-only entry point.
    #[test]
    fn require_init_phase_gates_on_the_marker() {
        let lua = Lua::new();

        let err = require_init_phase(&lua, "must be init").expect_err("runtime VM rejected");
        assert!(err.to_string().contains("must be init"));

        lua.set_app_data(InitPhase);
        assert!(
            require_init_phase(&lua, "must be init").is_ok(),
            "init VM allowed"
        );
    }

    /// Regression: an invalid chrono format string reached
    /// `DelayedFormat::to_string()`, which panics — a Lua caller could crash
    /// the callback. It must return a clean error instead.
    #[test]
    fn date_format_rejects_invalid_format_string() {
        let lua = Lua::new();
        for bad in ["%J", "%", "%Q"] {
            let err = util_date_format(&lua, LuaInt(0), bad.to_string())
                .expect_err("invalid format must error, not panic");
            assert!(
                err.to_string().contains("invalid date format"),
                "unexpected: {err}"
            );
        }
        // A valid format still works.
        assert_eq!(
            util_date_format(&lua, LuaInt(0), "%Y-%m-%d".to_string()).unwrap(),
            "1970-01-01"
        );
    }

    /// Regression: `date_add`/`date_diff` used unchecked i64 arithmetic —
    /// overflow panicked in debug and wrapped in release. Now a clean error.
    #[test]
    fn date_add_diff_reject_overflow() {
        let lua = Lua::new();
        assert!(util_date_add(&lua, LuaInt(i64::MAX), LuaInt(1)).is_err());
        assert!(util_date_diff(&lua, LuaInt(i64::MIN), LuaInt(1)).is_err());
        assert_eq!(util_date_add(&lua, LuaInt(100), LuaInt(5)).unwrap(), 105);
        assert_eq!(util_date_diff(&lua, LuaInt(100), LuaInt(40)).unwrap(), 60);
    }

    /// A fractional timestamp used to be silently truncated by the integer
    /// parameter conversion (`1.5` → `1`); it is rejected like every other
    /// integer the Lua API reads. A whole-valued float still works.
    #[test]
    fn date_helpers_reject_fractional_and_accept_whole_floats() {
        let lua = setup_lua();

        let err = lua
            .load("return crap.util.date_add(1.5, 1)")
            .exec()
            .unwrap_err()
            .to_string();
        assert!(err.contains("must be an integer"), "{err}");

        let err = lua
            .load("return crap.util.date_format(0.5, '%Y')")
            .exec()
            .unwrap_err()
            .to_string();
        assert!(err.contains("must be an integer"), "{err}");

        let sum: i64 = lua
            .load("return crap.util.date_add(2^3, 1)")
            .eval()
            .unwrap();
        assert_eq!(sum, 9);
    }

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

    fn lua_str(lua: &Lua, src: &str) -> String {
        lua.load(src).eval().unwrap()
    }

    /// `split` used to splice `sep` into a pattern character class, so a
    /// magic character errored and a multi-character separator split on any
    /// of its characters. It is a plain-string split.
    #[test]
    fn split_is_a_plain_string_split() {
        let lua = setup_lua();

        assert_eq!(
            lua_str(
                &lua,
                r#"return table.concat(crap.util.split("a%b", "%"), "|")"#
            ),
            "a|b"
        );
        assert_eq!(
            lua_str(
                &lua,
                r#"return table.concat(crap.util.split("a.b.c", "."), "|")"#
            ),
            "a|b|c"
        );
        assert_eq!(
            lua_str(
                &lua,
                r#"return table.concat(crap.util.split("a::b:c::d", "::"), "|")"#
            ),
            "a|b:c|d",
            "a multi-character separator splits on the whole sequence"
        );
        assert_eq!(
            lua_str(
                &lua,
                r#"return table.concat(crap.util.split("a,,b,", ","), "|")"#
            ),
            "a|b",
            "empty pieces are omitted, as before"
        );
        assert_eq!(
            lua_str(&lua, r#"return tostring(#crap.util.split("", ","))"#),
            "0"
        );
    }

    #[test]
    fn split_rejects_an_empty_separator() {
        let lua = setup_lua();
        let err = lua
            .load(r#"return crap.util.split("abc", "")"#)
            .exec()
            .unwrap_err()
            .to_string();
        assert!(err.contains("non-empty"), "{err}");
    }

    /// `truncate` counts characters, not bytes, and never returns more than
    /// `max_len` characters — a byte cut used to split a multi-byte character
    /// and a suffix longer than `max_len` used to return nearly the whole
    /// string plus the suffix.
    #[test]
    fn truncate_is_utf8_aware_and_never_exceeds_max_len() {
        let lua = setup_lua();

        assert_eq!(
            lua_str(&lua, r#"return crap.util.truncate("hello world", 8)"#),
            "hello..."
        );
        assert_eq!(
            lua_str(&lua, r#"return crap.util.truncate("hello world", 8, "~")"#),
            "hello w~"
        );
        assert_eq!(
            lua_str(&lua, r#"return crap.util.truncate("héllo", 10)"#),
            "héllo",
            "fits in characters even though it is longer in bytes"
        );
        assert_eq!(
            lua_str(&lua, r#"return crap.util.truncate("ééééé", 4, "…")"#),
            "ééé…",
            "cuts on a character boundary and counts the suffix in characters"
        );
        assert_eq!(
            lua_str(&lua, r#"return crap.util.truncate("hello world", 2)"#),
            "..",
            "a suffix longer than max_len is itself cut to max_len"
        );
        assert_eq!(
            lua_str(&lua, r#"return crap.util.truncate("hello world", 0)"#),
            ""
        );
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
