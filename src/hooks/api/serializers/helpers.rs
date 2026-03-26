//! Shared helpers for Lua table serializers.

use mlua::{Lua, Value};
use serde_json::{Map as JsonMap, Number as JsonNumber, Value as JsonValue};

use crate::core::LocalizedString;

/// Convert a LocalizedString to a Lua value (string or locale table).
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

const MAX_NESTING_DEPTH: usize = 64;

/// Convert a Lua value to a JSON value.
pub fn lua_to_json(lua: &Lua, value: &Value) -> mlua::Result<JsonValue> {
    lua_to_json_inner(lua, value, 0)
}

#[allow(clippy::only_used_in_recursion)]
fn lua_to_json_inner(lua: &Lua, value: &Value, depth: usize) -> mlua::Result<JsonValue> {
    if depth > MAX_NESTING_DEPTH {
        return Err(mlua::Error::RuntimeError(format!(
            "Table nesting exceeds maximum depth of {}",
            MAX_NESTING_DEPTH
        )));
    }

    match value {
        Value::Nil => Ok(JsonValue::Null),
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
                        map.insert(key, lua_to_json_inner(lua, &v, depth + 1)?);
                    }

                    Ok(JsonValue::Object(map))
                } else {
                    let mut arr = Vec::new();

                    for i in 1..=len {
                        let v: Value = t.raw_get(i)?;
                        arr.push(lua_to_json_inner(lua, &v, depth + 1)?);
                    }

                    Ok(JsonValue::Array(arr))
                }
            } else {
                let mut map = JsonMap::new();

                for pair in t.clone().pairs::<String, Value>() {
                    let (k, v) = pair?;
                    map.insert(k, lua_to_json_inner(lua, &v, depth + 1)?);
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
    if depth > MAX_NESTING_DEPTH {
        return Err(mlua::Error::RuntimeError(format!(
            "JSON nesting exceeds maximum depth of {}",
            MAX_NESTING_DEPTH
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
                Ok(Value::Nil)
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
    use crate::core::field::LocalizedString;
    use mlua::Lua;
    use serde_json::json;
    use std::collections::HashMap;

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
        let lua = Lua::new();
        let result = lua_to_json(&lua, &Value::Nil).unwrap();
        assert_eq!(result, json!(null));
    }

    #[test]
    fn test_lua_to_json_boolean() {
        let lua = Lua::new();
        let result = lua_to_json(&lua, &Value::Boolean(true)).unwrap();
        assert_eq!(result, json!(true));
        let result = lua_to_json(&lua, &Value::Boolean(false)).unwrap();
        assert_eq!(result, json!(false));
    }

    #[test]
    fn test_lua_to_json_integer() {
        let lua = Lua::new();
        let result = lua_to_json(&lua, &Value::Integer(42)).unwrap();
        assert_eq!(result, json!(42));
        let result = lua_to_json(&lua, &Value::Integer(-1)).unwrap();
        assert_eq!(result, json!(-1));
    }

    #[test]
    fn test_lua_to_json_number() {
        let lua = Lua::new();
        let result = lua_to_json(&lua, &Value::Number(3.15)).unwrap();
        assert_eq!(result, json!(3.15));
    }

    #[test]
    fn test_lua_to_json_string() {
        let lua = Lua::new();
        let s = lua.create_string("hello world").unwrap();
        let result = lua_to_json(&lua, &Value::String(s)).unwrap();
        assert_eq!(result, json!("hello world"));
    }

    #[test]
    fn test_lua_to_json_array_table() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set(1, "a").unwrap();
        tbl.set(2, "b").unwrap();
        tbl.set(3, "c").unwrap();
        let result = lua_to_json(&lua, &Value::Table(tbl)).unwrap();
        assert_eq!(result, json!(["a", "b", "c"]));
    }

    #[test]
    fn test_lua_to_json_object_table() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("name", "test").unwrap();
        tbl.set("count", 42).unwrap();
        let result = lua_to_json(&lua, &Value::Table(tbl)).unwrap();
        assert_eq!(result["name"], json!("test"));
        assert_eq!(result["count"], json!(42));
    }

    #[test]
    fn test_lua_to_json_function_becomes_null() {
        let lua = Lua::new();
        let f = lua.create_function(|_, ()| Ok(())).unwrap();
        let result = lua_to_json(&lua, &Value::Function(f)).unwrap();
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

        let err = lua_to_json(&lua, &val).unwrap_err();
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

        let result = lua_to_json(&lua, &Value::Table(tbl)).unwrap();
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
        let back = lua_to_json(&lua, &lua_val).unwrap();
        assert_eq!(back["title"], json!("Hello"));
        assert_eq!(back["count"], json!(42));
        assert_eq!(back["tags"], json!(["a", "b"]));
        assert_eq!(back["active"], json!(true));
        assert_eq!(back["empty"], json!(null));
    }
}
