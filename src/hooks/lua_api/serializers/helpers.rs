//! Shared helpers for Lua table serializers.

use std::sync::OnceLock;

use mlua::{Lua, Table, Value};
use serde_json::{Map as JsonMap, Number as JsonNumber, Value as JsonValue};

use crate::core::{HookRef, LocalizedString};

/// Serialize a [`HookRef`] back to Lua — a bare string when it carries no
/// options, or a `{ ref, options }` table otherwise. Inverse of the parse-side
/// `parse_hook_ref`, so a config round-trips through serialize→parse unchanged.
pub(super) fn hook_ref_to_lua(lua: &Lua, hook: &HookRef) -> mlua::Result<Value> {
    let Some(options) = hook.options() else {
        return Ok(Value::String(lua.create_string(hook.reference())?));
    };

    let tbl = lua.create_table()?;
    tbl.set("ref", hook.reference())?;
    tbl.set("options", json_to_lua(lua, options)?)?;

    Ok(Value::Table(tbl))
}

/// Serialize a list of [`HookRef`]s to a Lua array (each entry a string or
/// `{ ref, options }` table).
pub(super) fn hook_ref_list_to_lua(lua: &Lua, hooks: &[HookRef]) -> mlua::Result<Table> {
    let tbl = lua.create_table()?;

    for (i, hook) in hooks.iter().enumerate() {
        tbl.set(i + 1, hook_ref_to_lua(lua, hook)?)?;
    }

    Ok(tbl)
}

/// Convert a `LocalizedString` to a Lua value (string or locale table).
pub(super) fn localized_string_to_lua(lua: &Lua, ls: &LocalizedString) -> mlua::Result<Value> {
    match ls {
        LocalizedString::Plain(s) => Ok(Value::String(lua.create_string(s)?)),
        LocalizedString::Localized(map) => {
            let tbl = lua.create_table()?;

            for (k, v) in map {
                tbl.set(k.as_str(), v.as_str())?;
            }

            Ok(Value::Table(tbl))
        }
    }
}

/// Fallback nesting limit used before config is applied (and in unit tests /
/// CLI paths that never call [`set_max_nesting_depth`]). Matches the
/// `depth.max_nesting_depth` config default.
const DEFAULT_MAX_NESTING_DEPTH: usize = 64;

/// Process-wide JSON data-nesting limit, set once from `depth.max_nesting_depth`
/// at server startup (after config validation). Distinct from relationship
/// population depth.
static MAX_NESTING_DEPTH: OnceLock<usize> = OnceLock::new();

/// Apply the configured data-nesting limit (called once at startup). Subsequent
/// calls are ignored — the limit is fixed for the process lifetime.
pub fn set_max_nesting_depth(limit: usize) {
    let _ = MAX_NESTING_DEPTH.set(limit);
}

fn max_nesting_depth() -> usize {
    MAX_NESTING_DEPTH
        .get()
        .copied()
        .unwrap_or(DEFAULT_MAX_NESTING_DEPTH)
}

/// Convert a Lua value to a JSON value.
pub fn lua_to_json(value: &Value) -> mlua::Result<JsonValue> {
    lua_to_json_inner(value, 0)
}

fn lua_to_json_inner(value: &Value, depth: usize) -> mlua::Result<JsonValue> {
    let max = max_nesting_depth();
    if depth > max {
        return Err(mlua::Error::RuntimeError(format!(
            "Table nesting exceeds maximum depth of {max}"
        )));
    }

    match value {
        Value::Boolean(b) => Ok(JsonValue::Bool(*b)),
        Value::Integer(i) => Ok(JsonValue::Number((*i).into())),
        Value::Number(n) => JsonNumber::from_f64(*n)
            .map(JsonValue::Number)
            .ok_or_else(|| mlua::Error::RuntimeError("Invalid float value".into())),
        Value::String(s) => Ok(JsonValue::String(s.to_str()?.to_string())),
        Value::Table(t) => {
            let len = t.raw_len();

            if len > 0 {
                let has_string_keys = t
                    .clone()
                    .pairs::<Value, Value>()
                    .any(|pair| matches!(pair, Ok((Value::String(_), _))));

                if has_string_keys {
                    let mut map = JsonMap::new();

                    for pair in t.clone().pairs::<Value, Value>() {
                        let (k, v) = pair?;
                        let key = match k {
                            Value::String(s) => s.to_str()?.to_string(),
                            Value::Integer(i) => i.to_string(),
                            Value::Number(n) => n.to_string(),
                            _ => continue,
                        };
                        map.insert(key, lua_to_json_inner(&v, depth + 1)?);
                    }

                    Ok(JsonValue::Object(map))
                } else {
                    let mut arr = Vec::new();

                    for i in 1..=len {
                        let v: Value = t.raw_get(i)?;
                        arr.push(lua_to_json_inner(&v, depth + 1)?);
                    }

                    Ok(JsonValue::Array(arr))
                }
            } else {
                let mut map = JsonMap::new();

                for pair in t.clone().pairs::<String, Value>() {
                    let (k, v) = pair?;
                    map.insert(k, lua_to_json_inner(&v, depth + 1)?);
                }

                Ok(JsonValue::Object(map))
            }
        }
        _ => Ok(JsonValue::Null),
    }
}

/// Convert a JSON value to a Lua value.
pub fn json_to_lua(lua: &Lua, value: &JsonValue) -> mlua::Result<Value> {
    json_to_lua_inner(lua, value, 0)
}

fn json_to_lua_inner(lua: &Lua, value: &JsonValue, depth: usize) -> mlua::Result<Value> {
    let max = max_nesting_depth();
    if depth > max {
        return Err(mlua::Error::RuntimeError(format!(
            "JSON nesting exceeds maximum depth of {max}"
        )));
    }

    match value {
        JsonValue::Null => Ok(Value::Nil),
        JsonValue::Bool(b) => Ok(Value::Boolean(*b)),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(Value::Integer(i))
            } else if let Some(f) = n.as_f64() {
                Ok(Value::Number(f))
            } else {
                Err(mlua::Error::RuntimeError(format!(
                    "JSON number {n} cannot be represented as i64 or f64"
                )))
            }
        }
        JsonValue::String(s) => Ok(Value::String(lua.create_string(s)?)),
        JsonValue::Array(arr) => {
            let tbl = lua.create_table()?;

            for (i, v) in arr.iter().enumerate() {
                tbl.set(i + 1, json_to_lua_inner(lua, v, depth + 1)?)?;
            }

            Ok(Value::Table(tbl))
        }
        JsonValue::Object(map) => {
            let tbl = lua.create_table()?;

            for (k, v) in map {
                tbl.set(k.as_str(), json_to_lua_inner(lua, v, depth + 1)?)?;
            }

            Ok(Value::Table(tbl))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::LocalizedString;
    use mlua::Lua;
    use serde_json::json;
    use std::collections::HashMap;

    /// `hook_ref_to_lua` emits a bare string for a ref with no options, and a
    /// `{ ref, options }` table otherwise — the inverse of the parse-side
    /// `parse_hook_ref`, so a config round-trips serialize→parse unchanged.
    #[test]
    fn hook_ref_to_lua_bare_and_with_options() {
        let lua = Lua::new();

        match hook_ref_to_lua(&lua, &HookRef::new("hooks.x")).unwrap() {
            Value::String(s) => assert_eq!(s.to_str().unwrap(), "hooks.x"),
            other => panic!("bare ref should serialize to a string, got {other:?}"),
        }

        match hook_ref_to_lua(&lua, &HookRef::with_options("hooks.y", json!({ "k": 1 }))).unwrap() {
            Value::Table(tbl) => {
                assert_eq!(tbl.get::<String>("ref").unwrap(), "hooks.y");
                let opts = tbl.get::<Table>("options").unwrap();
                assert_eq!(opts.get::<i64>("k").unwrap(), 1);
            }
            other => panic!("ref with options should serialize to a table, got {other:?}"),
        }
    }

    #[test]
    fn test_localized_string_plain() {
        let lua = Lua::new();
        let ls = LocalizedString::Plain("Hello".to_string());
        let result = localized_string_to_lua(&lua, &ls).unwrap();
        match result {
            Value::String(s) => assert_eq!(s.to_str().unwrap(), "Hello"),
            _ => panic!("Expected String"),
        }
    }

    #[test]
    fn test_localized_string_localized() {
        let lua = Lua::new();
        let mut map = HashMap::new();
        map.insert("en".to_string(), "Hello".to_string());
        map.insert("de".to_string(), "Hallo".to_string());
        let ls = LocalizedString::Localized(map);
        let result = localized_string_to_lua(&lua, &ls).unwrap();
        match result {
            Value::Table(tbl) => {
                let en: String = tbl.get("en").unwrap();
                let de: String = tbl.get("de").unwrap();
                assert_eq!(en, "Hello");
                assert_eq!(de, "Hallo");
            }
            _ => panic!("Expected Table"),
        }
    }

    #[test]
    fn test_lua_to_json_nil() {
        let result = lua_to_json(&Value::Nil).unwrap();
        assert_eq!(result, json!(null));
    }

    #[test]
    fn test_lua_to_json_boolean() {
        let result = lua_to_json(&Value::Boolean(true)).unwrap();
        assert_eq!(result, json!(true));
        let result = lua_to_json(&Value::Boolean(false)).unwrap();
        assert_eq!(result, json!(false));
    }

    #[test]
    fn test_lua_to_json_integer() {
        let result = lua_to_json(&Value::Integer(42)).unwrap();
        assert_eq!(result, json!(42));
        let result = lua_to_json(&Value::Integer(-1)).unwrap();
        assert_eq!(result, json!(-1));
    }

    #[test]
    fn test_lua_to_json_number() {
        let result = lua_to_json(&Value::Number(3.15)).unwrap();
        assert_eq!(result, json!(3.15));
    }

    #[test]
    fn test_lua_to_json_string() {
        let lua = Lua::new();
        let s = lua.create_string("hello world").unwrap();
        let result = lua_to_json(&Value::String(s)).unwrap();
        assert_eq!(result, json!("hello world"));
    }

    #[test]
    fn test_lua_to_json_array_table() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set(1, "a").unwrap();
        tbl.set(2, "b").unwrap();
        tbl.set(3, "c").unwrap();
        let result = lua_to_json(&Value::Table(tbl)).unwrap();
        assert_eq!(result, json!(["a", "b", "c"]));
    }

    #[test]
    fn test_lua_to_json_object_table() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("name", "test").unwrap();
        tbl.set("count", 42).unwrap();
        let result = lua_to_json(&Value::Table(tbl)).unwrap();
        assert_eq!(result["name"], json!("test"));
        assert_eq!(result["count"], json!(42));
    }

    #[test]
    fn test_lua_to_json_function_becomes_null() {
        let lua = Lua::new();
        let f = lua.create_function(|_, ()| Ok(())).unwrap();
        let result = lua_to_json(&Value::Function(f)).unwrap();
        assert_eq!(result, json!(null));
    }

    #[test]
    fn test_json_to_lua_null() {
        let lua = Lua::new();
        let result = json_to_lua(&lua, &json!(null)).unwrap();
        assert!(matches!(result, Value::Nil));
    }

    #[test]
    fn test_json_to_lua_bool() {
        let lua = Lua::new();
        let result = json_to_lua(&lua, &json!(true)).unwrap();
        assert!(matches!(result, Value::Boolean(true)));
    }

    #[test]
    fn test_json_to_lua_integer() {
        let lua = Lua::new();
        let result = json_to_lua(&lua, &json!(42)).unwrap();
        assert!(matches!(result, Value::Integer(42)));
    }

    #[test]
    fn test_json_to_lua_float() {
        let lua = Lua::new();
        let result = json_to_lua(&lua, &json!(3.15)).unwrap();
        match result {
            Value::Number(n) => assert!((n - 3.15).abs() < f64::EPSILON),
            _ => panic!("Expected Number"),
        }
    }

    #[test]
    fn test_json_to_lua_string() {
        let lua = Lua::new();
        let result = json_to_lua(&lua, &json!("hello")).unwrap();
        match result {
            Value::String(s) => assert_eq!(s.to_str().unwrap(), "hello"),
            _ => panic!("Expected String"),
        }
    }

    #[test]
    fn test_json_to_lua_array() {
        let lua = Lua::new();
        let result = json_to_lua(&lua, &json!([1, 2, 3])).unwrap();
        match result {
            Value::Table(tbl) => {
                assert_eq!(tbl.raw_len(), 3);
                let v1: i64 = tbl.get(1).unwrap();
                let v2: i64 = tbl.get(2).unwrap();
                let v3: i64 = tbl.get(3).unwrap();
                assert_eq!(v1, 1);
                assert_eq!(v2, 2);
                assert_eq!(v3, 3);
            }
            _ => panic!("Expected Table"),
        }
    }

    #[test]
    fn test_json_to_lua_object() {
        let lua = Lua::new();
        let result = json_to_lua(&lua, &json!({"name": "test", "active": true})).unwrap();
        match result {
            Value::Table(tbl) => {
                let name: String = tbl.get("name").unwrap();
                let active: bool = tbl.get("active").unwrap();
                assert_eq!(name, "test");
                assert!(active);
            }
            _ => panic!("Expected Table"),
        }
    }

    #[test]
    fn lua_to_json_rejects_deep_nesting() {
        let lua = Lua::new();
        let val = lua
            .load(
                r#"
                local t = {val = "leaf"}
                for i = 1, 70 do
                    t = {nested = t}
                end
                return t
            "#,
            )
            .eval::<Value>()
            .unwrap();

        let err = lua_to_json(&val).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("nesting exceeds maximum depth"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn json_to_lua_rejects_deep_nesting() {
        let lua = Lua::new();
        let mut val = json!({"val": "leaf"});

        for _ in 0..70 {
            val = json!({"nested": val});
        }

        let err = json_to_lua(&lua, &val).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("nesting exceeds maximum depth"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn lua_to_json_mixed_keys_becomes_object() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set(1, "first").unwrap();
        tbl.set(2, "second").unwrap();
        tbl.set("name", "test").unwrap();

        let result = lua_to_json(&Value::Table(tbl)).unwrap();
        assert!(result.is_object(), "expected object, got: {result}");
        assert_eq!(result["name"], json!("test"));
        assert_eq!(result["1"], json!("first"));
        assert_eq!(result["2"], json!("second"));
    }

    #[test]
    fn test_json_lua_roundtrip() {
        let lua = Lua::new();
        let original = json!({
            "title": "Hello",
            "count": 42,
            "tags": ["a", "b"],
            "active": true,
            "empty": null
        });
        let lua_val = json_to_lua(&lua, &original).unwrap();
        let back = lua_to_json(&lua_val).unwrap();
        assert_eq!(back["title"], json!("Hello"));
        assert_eq!(back["count"], json!(42));
        assert_eq!(back["tags"], json!(["a", "b"]));
        assert_eq!(back["active"], json!(true));
        assert_eq!(back["empty"], json!(null));
    }

    /// Edge-case numbers (`i64::MAX`, `f64::MAX`) must survive conversion without error.
    #[test]
    fn json_to_lua_extreme_numbers_succeed() {
        let lua = Lua::new();

        // Construct a JSON number from raw that can't be i64 or f64.
        // serde_json::Number doesn't easily allow this in normal usage,
        // but we can test via the u64::MAX path (which has no i64 representation
        // but does have an f64 representation with precision loss).
        // Instead, verify that normal edge cases still work:
        let big_int = json!(i64::MAX);
        let result = json_to_lua(&lua, &big_int);
        assert!(result.is_ok(), "i64::MAX should be representable");

        let big_float = json!(f64::MAX);
        let result = json_to_lua(&lua, &big_float);
        assert!(result.is_ok(), "f64::MAX should be representable");
    }
}
