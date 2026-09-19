//! Whole-number reading from Lua values — the one rule every integer option
//! and integer parameter of the `crap.*` API follows.
//!
//! Lua 5.4 has two number subtypes and arithmetic readily produces floats
//! (`2^16` is `65536.0`), so a number with no fractional part is a
//! legitimate integer and is accepted. A fractional float is rejected rather
//! than truncated, and so is every non-number: an option the author wrote
//! must never be silently rounded or silently ignored.

use mlua::{Error::RuntimeError, FromLua, Lua, Result as LuaResult, Table, Value};

/// Read `value` as an integer. `what` names the value in the error
/// (`"priority"`, `"jobs.list_runs options 'limit'"`).
///
/// # Errors
///
/// A fractional float, or any value that is not a number.
pub(crate) fn lua_integer(lua: &Lua, value: &Value, what: &str) -> LuaResult<i64> {
    match value {
        Value::Integer(i) => Ok(*i),
        // `coerce_integer` applies Lua's own float→integer rule: an exact
        // whole value in range converts, anything else does not.
        Value::Number(n) => lua
            .coerce_integer(Value::Number(*n))?
            .ok_or_else(|| RuntimeError(format!("{what} must be an integer, got {n}"))),
        other => Err(RuntimeError(format!(
            "{what} must be an integer, got {}",
            other.type_name()
        ))),
    }
}

/// Read the optional integer at `tbl[key]`: absent / `nil` → `None`, an
/// integer (or whole-valued float) → `Some`, anything else → an error naming
/// `context` and `key`.
///
/// # Errors
///
/// See [`lua_integer`].
pub(crate) fn opt_integer(
    lua: &Lua,
    tbl: &Table,
    key: &str,
    context: &str,
) -> LuaResult<Option<i64>> {
    let value: Value = tbl.get(key)?;

    if matches!(value, Value::Nil) {
        return Ok(None);
    }

    lua_integer(lua, &value, &format!("{context} '{key}'")).map(Some)
}

/// An integer parameter of a `#[lua_fn]` function. mlua's own `i64`
/// conversion truncates a fractional float (`1.5` → `1`); this wrapper
/// applies [`lua_integer`] instead, so a fractional argument is an error.
/// Declare the parameter with `#[lua(ty = "integer")]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LuaInt(pub(crate) i64);

impl FromLua for LuaInt {
    fn from_lua(value: Value, lua: &Lua) -> LuaResult<Self> {
        lua_integer(lua, &value, "argument").map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(lua: &Lua, src: &str) -> Value {
        lua.load(src).eval().unwrap()
    }

    #[test]
    fn integer_passes_through() {
        let lua = Lua::new();
        assert_eq!(
            lua_integer(&lua, &value(&lua, "return 42"), "n").unwrap(),
            42
        );
        assert_eq!(
            lua_integer(&lua, &value(&lua, "return -7"), "n").unwrap(),
            -7
        );
    }

    /// `2^16` is a float in Lua 5.4; a whole value is a legitimate integer.
    #[test]
    fn whole_valued_float_is_accepted() {
        let lua = Lua::new();
        assert_eq!(
            lua_integer(&lua, &value(&lua, "return 2^16"), "n").unwrap(),
            65_536
        );
        assert_eq!(
            lua_integer(&lua, &value(&lua, "return 8.0"), "n").unwrap(),
            8
        );
    }

    /// A fractional float is rejected — never truncated to `1`.
    #[test]
    fn fractional_float_is_rejected() {
        let lua = Lua::new();
        let err = lua_integer(&lua, &value(&lua, "return 1.5"), "priority")
            .unwrap_err()
            .to_string();
        assert!(err.contains("priority must be an integer"), "{err}");
        assert!(err.contains("1.5"), "names the offending value: {err}");
    }

    #[test]
    fn out_of_range_and_non_finite_floats_are_rejected() {
        let lua = Lua::new();
        assert!(lua_integer(&lua, &value(&lua, "return 2^70"), "n").is_err());
        assert!(lua_integer(&lua, &value(&lua, "return 1/0"), "n").is_err());
        assert!(lua_integer(&lua, &value(&lua, "return 0/0"), "n").is_err());
    }

    /// Strings are not coerced (`"5"` is not an integer option value).
    #[test]
    fn non_numbers_are_rejected_naming_the_type() {
        let lua = Lua::new();
        let err = lua_integer(&lua, &value(&lua, "return '5'"), "limit")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("limit must be an integer, got string"),
            "{err}"
        );

        let err = lua_integer(&lua, &value(&lua, "return {}"), "limit")
            .unwrap_err()
            .to_string();
        assert!(err.contains("got table"), "{err}");
    }

    #[test]
    fn opt_integer_absent_is_none_and_present_is_typed() {
        let lua = Lua::new();
        let tbl: Table = lua
            .load("return { limit = 2^4, bad = 'x' }")
            .eval()
            .unwrap();

        assert_eq!(opt_integer(&lua, &tbl, "offset", "opts").unwrap(), None);
        assert_eq!(opt_integer(&lua, &tbl, "limit", "opts").unwrap(), Some(16));

        let err = opt_integer(&lua, &tbl, "bad", "jobs.list_runs options")
            .unwrap_err()
            .to_string();
        assert!(err.contains("jobs.list_runs options 'bad'"), "{err}");
    }

    #[test]
    fn lua_int_param_rejects_fractional_and_accepts_whole() {
        let lua = Lua::new();
        let echo = lua.create_function(|_, n: LuaInt| Ok(n.0)).unwrap();
        lua.globals().set("echo", echo).unwrap();

        let whole: i64 = lua.load("return echo(2^3)").eval().unwrap();
        assert_eq!(whole, 8);

        let err = lua.load("return echo(1.5)").exec().unwrap_err().to_string();
        assert!(err.contains("must be an integer"), "{err}");
    }
}
