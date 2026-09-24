//! Shared helpers for Lua table serializers.

use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table, Value};
use serde::Serialize;
use serde_json::{Map as JsonMap, Number as JsonNumber, Value as JsonValue, to_value};

use crate::core::{HookRef, LocalizedString, max_nesting_depth};

/// Serialize a [`HookRef`] back to Lua — a bare string when it carries no
/// options, or a `{ ref, options }` table otherwise. Inverse of the parse-side
/// `parse_hook_ref`, so a config round-trips through serialize→parse unchanged.
pub(super) fn hook_ref_to_lua(lua: &Lua, hook: &HookRef) -> LuaResult<Value> {
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
pub(super) fn hook_ref_list_to_lua(lua: &Lua, hooks: &[HookRef]) -> LuaResult<Table> {
    let tbl = lua.create_table()?;

    for (i, hook) in hooks.iter().enumerate() {
        tbl.set(i + 1, hook_ref_to_lua(lua, hook)?)?;
    }

    Ok(tbl)
}

/// Convert a `LocalizedString` to a Lua value (string or locale table).
pub(super) fn localized_string_to_lua(lua: &Lua, ls: &LocalizedString) -> LuaResult<Value> {
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

/// Convert a Lua value to a JSON value.
///
/// `nil` and the `crap.null` sentinel both become `null`. `crap.null` is
/// mlua's null light-userdata ([`Value::NULL`]) — the same value the serde
/// deserializer behind `lua.from_value` reads as `null` — and since
/// assigning `nil` erases a table key, it is the only way a script keeps a
/// present-null key or array slot.
pub fn lua_to_json(value: &Value) -> LuaResult<JsonValue> {
    lua_to_json_inner(value, 0)
}

fn lua_to_json_inner(value: &Value, depth: usize) -> LuaResult<JsonValue> {
    let max = max_nesting_depth();
    if depth > max {
        return Err(RuntimeError(format!(
            "Table nesting exceeds maximum depth of {max}"
        )));
    }

    match value {
        Value::Boolean(b) => Ok(JsonValue::Bool(*b)),
        Value::Integer(i) => Ok(JsonValue::Number((*i).into())),
        Value::Number(n) => JsonNumber::from_f64(*n)
            .map(JsonValue::Number)
            .ok_or_else(|| RuntimeError("Invalid float value".into())),
        Value::String(s) => Ok(JsonValue::String(s.to_str()?.to_string())),
        Value::Table(t) => lua_table_to_json(t, depth),
        // `nil`, the `crap.null` sentinel, and non-data values (functions,
        // userdata, threads).
        _ => Ok(JsonValue::Null),
    }
}

/// Convert a Lua table: a non-empty sequence without string keys is an
/// array, anything else an object.
fn lua_table_to_json(t: &Table, depth: usize) -> LuaResult<JsonValue> {
    let len = t.raw_len();

    if len == 0 {
        let mut map = JsonMap::new();

        for pair in t.pairs::<String, Value>() {
            let (k, v) = pair?;
            map.insert(k, lua_to_json_inner(&v, depth + 1)?);
        }

        return Ok(JsonValue::Object(map));
    }

    let has_string_keys = t
        .pairs::<Value, Value>()
        .any(|pair| matches!(pair, Ok((Value::String(_), _))));

    if !has_string_keys {
        let mut arr = Vec::with_capacity(len);

        for i in 1..=len {
            let v: Value = t.raw_get(i)?;
            arr.push(lua_to_json_inner(&v, depth + 1)?);
        }

        return Ok(JsonValue::Array(arr));
    }

    let mut map = JsonMap::new();

    for pair in t.pairs::<Value, Value>() {
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
}

/// Serialize a Rust value into the Lua value a script reads. The one
/// Rust→Lua serde conversion: every context, argument and result table
/// handed to Lua goes through it, and it shares [`json_to_lua`]'s null
/// rules — a null object field (JSON `null`, absent `Option`, unit) is
/// `nil`, a null array element is `crap.null` — so every surface agrees.
/// (mlua's own serializer would emit its truthy null light-userdata for
/// every null, making `if ctx.data.x then` take the branch for a null
/// field.)
pub fn to_lua_value<T: Serialize + ?Sized>(lua: &Lua, value: &T) -> LuaResult<Value> {
    let json = to_value(value).map_err(|e| RuntimeError(format!("serialize error: {e:#}")))?;

    json_to_lua(lua, &json)
}

/// Convert a JSON value to a Lua value.
///
/// Null handling: a bare `null` and a null object field are `nil` (the key is
/// simply absent), while a null **array element** becomes `crap.null` so the
/// array keeps its length — a `nil` slot would leave a hole that truncates
/// `#t` / `ipairs` and every later conversion back to JSON.
pub fn json_to_lua(lua: &Lua, value: &JsonValue) -> LuaResult<Value> {
    json_to_lua_inner(lua, value, 0)
}

fn json_to_lua_inner(lua: &Lua, value: &JsonValue, depth: usize) -> LuaResult<Value> {
    let max = max_nesting_depth();
    if depth > max {
        return Err(RuntimeError(format!(
            "JSON nesting exceeds maximum depth of {max}"
        )));
    }

    match value {
        JsonValue::Null => Ok(Value::Nil),
        JsonValue::Bool(b) => Ok(Value::Boolean(*b)),
        JsonValue::Number(n) => json_number_to_lua(n),
        JsonValue::String(s) => Ok(Value::String(lua.create_string(s)?)),
        JsonValue::Array(arr) => {
            let tbl = lua.create_table_with_capacity(arr.len(), 0)?;

            for (i, v) in arr.iter().enumerate() {
                let element = match v {
                    JsonValue::Null => Value::NULL,
                    other => json_to_lua_inner(lua, other, depth + 1)?,
                };
                tbl.raw_set(i + 1, element)?;
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

/// A JSON number as a Lua integer when it fits `i64`, else a float.
fn json_number_to_lua(n: &JsonNumber) -> LuaResult<Value> {
    if let Some(i) = n.as_i64() {
        return Ok(Value::Integer(i));
    }

    n.as_f64().map(Value::Number).ok_or_else(|| {
        RuntimeError(format!(
            "JSON number {n} cannot be represented as i64 or f64"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::LocalizedString;
    use mlua::LuaSerdeExt;
    use proptest::prelude::*;
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

    /// Every null shape a serialized context can carry in an object field —
    /// a JSON null inside a map, an absent `Option`, a unit — reaches Lua as
    /// `nil`, never as the truthy null light-userdata mlua emits by default.
    /// A null *array element* is the `crap.null` sentinel instead, so the
    /// array keeps its length.
    #[test]
    fn to_lua_value_maps_null_fields_to_nil_and_null_elements_to_the_sentinel() {
        #[derive(Serialize)]
        struct Ctx {
            data: JsonValue,
            user: Option<String>,
            unit: (),
        }

        let lua = Lua::new();
        let ctx = Ctx {
            data: json!({ "x": null, "list": [1, null, 3] }),
            user: None,
            unit: (),
        };
        lua.globals()
            .set("ctx", to_lua_value(&lua, &ctx).unwrap())
            .unwrap();
        lua.globals().set("null", Value::NULL).unwrap();

        let all_nil: bool = lua
            .load(
                "return ctx.data.x == nil and ctx.user == nil and ctx.unit == nil \
                 and not ctx.data.x",
            )
            .eval()
            .unwrap();
        assert!(all_nil, "a null object field must reach Lua as nil");

        let list_intact: bool = lua
            .load(
                "return #ctx.data.list == 3 and ctx.data.list[2] == null and ctx.data.list[3] == 3",
            )
            .eval()
            .unwrap();
        assert!(list_intact, "a null array element must keep its slot");
    }

    /// `crap.null` (mlua's null light-userdata) is JSON `null` on every
    /// Lua→Rust path: in an object field (where `nil` would erase the key),
    /// in an array slot, bare, and through the serde deserializer behind
    /// `lua.from_value`.
    #[test]
    fn the_null_sentinel_converts_to_json_null() {
        let lua = Lua::new();
        lua.globals().set("null", Value::NULL).unwrap();

        let tbl: Value = lua
            .load("return { cleared = null, kept = 1, list = { 1, null, 3 } }")
            .eval()
            .unwrap();

        let expected = json!({ "cleared": null, "kept": 1, "list": [1, null, 3] });
        assert_eq!(lua_to_json(&tbl).unwrap(), expected);
        assert_eq!(lua_to_json(&Value::NULL).unwrap(), json!(null));

        let via_serde: JsonValue = lua.from_value(tbl).unwrap();
        assert_eq!(via_serde, expected);
    }

    /// A JSON array with null elements keeps its length in Lua (`#t`,
    /// `ipairs`), each null slot holding the sentinel.
    #[test]
    fn json_to_lua_keeps_null_array_elements() {
        let lua = Lua::new();
        let Value::Table(tbl) = json_to_lua(&lua, &json!([null, "a", null])).unwrap() else {
            panic!("expected a table");
        };

        assert_eq!(tbl.raw_len(), 3);
        assert_eq!(tbl.raw_get::<Value>(1).unwrap(), Value::NULL);
        assert_eq!(tbl.raw_get::<String>(2).unwrap(), "a");
        assert_eq!(tbl.raw_get::<Value>(3).unwrap(), Value::NULL);
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
        // Indexing a missing key yields `Value::Null`, so comparing
        // `back["empty"]` against `json!(null)` would pass whether the key
        // survived or vanished. Assert on the key set instead: Lua drops it.
        assert!(
            !back.as_object().expect("object").contains_key("empty"),
            "a null-valued key is erased by Lua, not kept as null: {back}"
        );
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

    // ── json_to_lua → lua_to_json round-trip ──────────────────────────────
    //
    // This pair is the data boundary every Lua hook, job handler and CRUD
    // call crosses, so its identities AND its three non-identities are
    // pinned here. Whole-value `assert_eq!` compares key sets, which a
    // per-key lookup cannot: a vanished key reads back as `null`.

    /// Send a JSON value through Lua and back.
    fn round_trip(value: &JsonValue) -> JsonValue {
        let lua = Lua::new();
        let as_lua = json_to_lua(&lua, value).expect("json_to_lua must succeed");

        lua_to_json(&as_lua).expect("lua_to_json must succeed")
    }

    /// Object keys that must survive: plain identifiers, digit-only keys
    /// (which must stay *string* keys rather than turning into array
    /// indices), and non-ASCII.
    fn json_key() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("title".to_owned()),
            Just("nested_value".to_owned()),
            Just("7".to_owned()),
            Just("42".to_owned()),
            Just("ümlaut".to_owned()),
            Just("日本".to_owned()),
        ]
    }

    /// Scalars that must survive: both number representations (an integer
    /// must stay an integer and a float must stay a float), booleans, and
    /// strings carrying quotes, backslashes, control whitespace and
    /// multi-byte characters.
    fn json_scalar() -> impl Strategy<Value = JsonValue> {
        prop_oneof![
            any::<bool>().prop_map(JsonValue::Bool),
            any::<i64>().prop_map(|i| JsonValue::Number(i.into())),
            (-1e9f64..1e9f64)
                .prop_map(|f| JsonValue::Number(JsonNumber::from_f64(f).expect("finite float"))),
            Just(JsonValue::String(String::new())),
            Just(JsonValue::String("plain".to_owned())),
            Just(JsonValue::String(
                "quote\" back\\slash\ttab\nnewline".to_owned()
            )),
            Just(JsonValue::String("ümlaut 日本 😀".to_owned())),
        ]
    }

    /// JSON values that must survive the Lua round-trip unchanged, including
    /// null array elements (carried as the `crap.null` sentinel). Two shapes
    /// are deliberately absent because Lua cannot represent them — each is
    /// pinned by its own named test below:
    ///
    /// - a null *object field*, because assigning `nil` in Lua erases the key
    ///   it is stored under,
    /// - `[]`, because an empty Lua table is indistinguishable from `{}`.
    fn round_trippable_json() -> impl Strategy<Value = JsonValue> {
        json_scalar().prop_recursive(4, 32, 4, |inner| {
            let element = prop_oneof![4 => inner.clone(), 1 => Just(JsonValue::Null)];

            prop_oneof![
                prop::collection::vec(element, 1..4).prop_map(JsonValue::Array),
                prop::collection::vec((json_key(), inner), 0..4)
                    .prop_map(|entries| JsonValue::Object(entries.into_iter().collect())),
            ]
        })
    }

    proptest! {
        /// Property: every JSON value Lua *can* represent survives
        /// `json_to_lua` → `lua_to_json` byte-for-byte — nesting, key sets,
        /// array order, number representation and string contents alike.
        #[test]
        fn json_round_trips_through_lua(value in round_trippable_json()) {
            let back = round_trip(&value);

            prop_assert_eq!(back, value);
        }
    }

    /// Pinned non-identity: Lua has a single table type, so an empty array
    /// and an empty object are the same value once converted, and the
    /// round-trip reports `{}` for both. Changing this silently re-types
    /// every empty list that crosses the Lua boundary.
    #[test]
    fn empty_array_round_trips_as_empty_object() {
        assert_eq!(round_trip(&json!([])), json!({}));
        assert_eq!(round_trip(&json!({})), json!({}));
        assert_eq!(round_trip(&json!({ "tags": [] })), json!({ "tags": {} }));

        // A non-empty array is unambiguous and keeps its type.
        assert_eq!(
            round_trip(&json!({ "tags": ["a"] })),
            json!({ "tags": ["a"] })
        );
    }

    /// Pinned non-identity: assigning `nil` removes the key, so a
    /// present-null field does not survive — the key set shrinks rather
    /// than the value becoming null. Callers that must tell "absent" from
    /// "explicitly null" re-insert the null after the Lua leg (the
    /// present-null hook-context rule).
    #[test]
    fn a_null_valued_key_is_dropped() {
        let back = round_trip(&json!({ "kept": 1, "cleared": null }));

        assert_eq!(back, json!({ "kept": 1 }));
        assert!(
            !back.as_object().expect("object").contains_key("cleared"),
            "the key must be gone, not present-as-null: {back}"
        );
    }

    /// A bare null is Lua `nil`, which converts back to null, and a null
    /// array element is the `crap.null` sentinel, which does too — only a
    /// null stored under an object key is lost (pinned above).
    #[test]
    fn a_bare_null_and_a_null_element_round_trip() {
        assert_eq!(round_trip(&json!(null)), json!(null));
        assert_eq!(round_trip(&json!([null])), json!([null]));
        assert_eq!(
            round_trip(&json!({ "tags": ["a", null, "b"] })),
            json!({ "tags": ["a", null, "b"] })
        );
    }

    /// Pinned non-identity: an integer above `i64::MAX` has no Lua integer
    /// representation, so it degrades to a float — losing its integer type
    /// and, past 2^53, its exact value.
    #[test]
    fn an_integer_above_i64_max_becomes_a_float() {
        let back = round_trip(&json!(u64::MAX));

        assert!(back.is_f64(), "expected a float, got {back}");
        assert_ne!(back, json!(u64::MAX));
    }

    /// Integers at the `i64` boundaries stay integers — the degradation above
    /// starts exactly one step past `i64::MAX`.
    #[test]
    fn i64_boundary_integers_stay_integers() {
        for original in [json!(i64::MAX), json!(i64::MIN), json!(0), json!(-1)] {
            let back = round_trip(&original);

            assert_eq!(back, original);
            assert!(back.is_i64(), "expected an integer, got {back}");
        }
    }

    /// Objects inside arrays inside objects, both number kinds, and strings
    /// with escapes and multi-byte characters come back identical — key
    /// sets included, at every level.
    #[test]
    fn nested_mixed_structures_round_trip() {
        let original = json!({
            "rows": [
                { "id": 1, "ratio": 0.5, "tags": ["a", "b"] },
                { "id": 2, "ratio": -1.25, "tags": [["deep"], { "flag": true }] }
            ],
            "text": "quote\" back\\slash\nnewline 日本 😀",
            "whole_float": 3.0,
            "blank": "",
            "empty_object": {}
        });

        assert_eq!(round_trip(&original), original);
    }
}
