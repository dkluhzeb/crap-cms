//! Shared helpers for Lua table serializers.

use mlua::{Lua, Value};

/// Convert a LocalizedString to a Lua value (string or locale table).
pub(super) fn localized_string_to_lua(
    lua: &Lua,
    ls: &crate::core::field::LocalizedString,
) -> mlua::Result<Value> {
    match ls {
        crate::core::field::LocalizedString::Plain(s) => Ok(Value::String(lua.create_string(s)?)),
        crate::core::field::LocalizedString::Localized(map) => {
            let tbl = lua.create_table()?;
            for (k, v) in map {
                tbl.set(k.as_str(), v.as_str())?;
            }
            Ok(Value::Table(tbl))
        }
    }
}

/// Convert a Lua value to a serde_json::Value.
pub fn lua_to_json(_lua: &Lua, value: &Value) -> mlua::Result<serde_json::Value> {
    match value {
        Value::Nil => Ok(serde_json::Value::Null),
        Value::Boolean(b) => Ok(serde_json::Value::Bool(*b)),
        Value::Integer(i) => Ok(serde_json::Value::Number((*i).into())),
        Value::Number(n) => serde_json::Number::from_f64(*n)
            .map(serde_json::Value::Number)
            .ok_or_else(|| mlua::Error::RuntimeError("Invalid float value".into())),
        Value::String(s) => Ok(serde_json::Value::String(s.to_str()?.to_string())),
        Value::Table(t) => {
            let len = t.raw_len();
            if len > 0 {
                let mut arr = Vec::new();
                for i in 1..=len {
                    let v: Value = t.raw_get(i)?;
                    arr.push(lua_to_json(_lua, &v)?);
                }
                Ok(serde_json::Value::Array(arr))
            } else {
                let mut map = serde_json::Map::new();
                for pair in t.clone().pairs::<String, Value>() {
                    let (k, v) = pair?;
                    map.insert(k, lua_to_json(_lua, &v)?);
                }
                Ok(serde_json::Value::Object(map))
            }
        }
        _ => Ok(serde_json::Value::Null),
    }
}

/// Convert a serde_json::Value to a Lua value.
pub fn json_to_lua(lua: &Lua, value: &serde_json::Value) -> mlua::Result<Value> {
    match value {
        serde_json::Value::Null => Ok(Value::Nil),
        serde_json::Value::Bool(b) => Ok(Value::Boolean(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(Value::Integer(i))
            } else if let Some(f) = n.as_f64() {
                Ok(Value::Number(f))
            } else {
                Ok(Value::Nil)
            }
        }
        serde_json::Value::String(s) => Ok(Value::String(lua.create_string(s)?)),
        serde_json::Value::Array(arr) => {
            let tbl = lua.create_table()?;
            for (i, v) in arr.iter().enumerate() {
                tbl.set(i + 1, json_to_lua(lua, v)?)?;
            }
            Ok(Value::Table(tbl))
        }
        serde_json::Value::Object(map) => {
            let tbl = lua.create_table()?;
            for (k, v) in map {
                tbl.set(k.as_str(), json_to_lua(lua, v)?)?;
            }
            Ok(Value::Table(tbl))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Lua;
    use serde_json::json;

    #[test]
    fn test_localized_string_plain() {
        let lua = Lua::new();
        let ls = crate::core::field::LocalizedString::Plain("Hello".to_string());
        let result = localized_string_to_lua(&lua, &ls).unwrap();
        match result {
            Value::String(s) => assert_eq!(s.to_str().unwrap(), "Hello"),
            _ => panic!("Expected String"),
        }
    }

    #[test]
    fn test_localized_string_localized() {
        let lua = Lua::new();
        let mut map = std::collections::HashMap::new();
        map.insert("en".to_string(), "Hello".to_string());
        map.insert("de".to_string(), "Hallo".to_string());
        let ls = crate::core::field::LocalizedString::Localized(map);
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
        let result = lua_to_json(&lua, &Value::Number(3.14)).unwrap();
        assert_eq!(result, json!(3.14));
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
        let result = json_to_lua(&lua, &json!(3.14)).unwrap();
        match result {
            Value::Number(n) => assert!((n - 3.14).abs() < f64::EPSILON),
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
