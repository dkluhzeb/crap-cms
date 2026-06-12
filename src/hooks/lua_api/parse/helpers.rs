//! Shared helper functions for Lua table parsing.

use std::collections::HashMap;

use anyhow::{Result, bail};
use mlua::{Error::RuntimeError, Result as LuaResult, Table, Value};

use crate::{
    core::{HookRef, LocalizedString, SelectOption, collection::Hooks},
    hooks::lua_api::lua_to_json,
};

/// Reject any named key in `table` that is not present in `allowed`.
///
/// Brings hand-parsed Lua schema tables to parity with the strict
/// `#[serde(deny_unknown_fields)]` behavior of serde-backed config (jobs):
/// a typo'd key (`timestamp`, `requird`, `localised`) becomes a hard error
/// at load time instead of being silently dropped. Only string keys are
/// validated — integer/array entries are skipped. When the unknown key is a
/// near-miss of a valid one, the error suggests it.
pub(crate) fn deny_unknown_keys(table: &Table, context: &str, allowed: &[&str]) -> Result<()> {
    for pair in table.clone().pairs::<Value, Value>() {
        let (key, _) = pair?;

        let Value::String(key) = key else { continue };
        let key = key.to_str()?;
        let name: &str = &key;

        if allowed.contains(&name) {
            continue;
        }

        let suggestion = closest_key(name, allowed)
            .map(|c| format!(" (did you mean '{c}'?)"))
            .unwrap_or_default();

        bail!(
            "Unknown {context} config key '{name}'{suggestion}. Valid keys: {}",
            allowed.join(", ")
        );
    }

    Ok(())
}

/// Closest valid key to `key` within an edit distance of 2, for typo hints.
fn closest_key<'a>(key: &str, allowed: &[&'a str]) -> Option<&'a str> {
    allowed
        .iter()
        .map(|cand| (levenshtein(key, cand), *cand))
        .filter(|(dist, _)| *dist <= 2)
        .min_by_key(|(dist, _)| *dist)
        .map(|(_, cand)| cand)
}

/// Classic two-row Levenshtein edit distance over Unicode scalar values.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();

    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr: Vec<usize> = vec![0; b.len() + 1];

    for (i, ca) in a.iter().enumerate() {
        curr[0] = i + 1;

        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }

        std::mem::swap(&mut prev, &mut curr);
    }

    prev[b.len()]
}

pub(super) fn get_table(tbl: &Table, key: &str) -> mlua::Result<Table> {
    tbl.get(key)
}

pub(super) fn get_string(tbl: &Table, key: &str) -> Option<String> {
    tbl.get::<Option<String>>(key).ok().flatten()
}

/// Strict optional string: absent / `nil` → `None`, a string → `Some`, any
/// other present value → a hard error.
///
/// Unlike [`get_string`], a present-but-non-string value is NOT silently
/// dropped. Use this for hook *references* (e.g. access rules), where silently
/// discarding a value the author wrote — `read = some_function`, `read = true` —
/// would drop an access rule and is a security footgun, not a harmless typo.
pub(super) fn get_optional_string_ref(
    tbl: &Table,
    key: &str,
    context: &str,
) -> Result<Option<String>> {
    match tbl.get::<Value>(key)? {
        Value::Nil => Ok(None),
        Value::String(s) => Ok(Some(s.to_str()?.to_string())),
        other => bail!(
            "{context} '{key}' must be a string hook reference, got {}",
            other.type_name()
        ),
    }
}

/// Parse a Lua value that is either a plain string or a `{locale = string}` table.
pub(super) fn get_localized_string(tbl: &Table, key: &str) -> Option<LocalizedString> {
    match tbl.get::<Value>(key) {
        Ok(Value::String(s)) => Some(LocalizedString::Plain(s.to_str().ok()?.to_string())),
        Ok(Value::Table(t)) => {
            let mut map = HashMap::new();

            for (k, v) in t.pairs::<String, String>().flatten() {
                map.insert(k, v);
            }

            if map.is_empty() {
                None
            } else {
                Some(LocalizedString::Localized(map))
            }
        }
        _ => None,
    }
}

/// Strict optional string: absent / `nil` → `None`, a string → `Some`,
/// numbers are coerced Lua-style (`50` → `"50"`); any other present value →
/// a hard error naming the key and the actual type.
///
/// Unlike [`get_string`], a present-but-wrong-typed value is NOT silently
/// dropped.
pub(super) fn get_string_strict(
    tbl: &Table,
    key: &str,
    context: &str,
) -> LuaResult<Option<String>> {
    match tbl.get::<Value>(key)? {
        Value::Nil => Ok(None),
        Value::String(s) => Ok(Some(s.to_str()?.to_string())),
        Value::Integer(i) => Ok(Some(i.to_string())),
        Value::Number(n) => Ok(Some(n.to_string())),
        other => Err(RuntimeError(format!(
            "{context} '{key}' must be a string, got {}",
            other.type_name()
        ))),
    }
}

/// Strict variant of [`get_localized_string`]: absent / `nil` → `None`, a
/// string → `Plain`, a non-empty table of `locale = "string"` pairs →
/// `Localized`. Anything else — wrong type, empty table, non-string pair —
/// is a hard error instead of a silent drop.
pub(super) fn get_localized_string_strict(
    tbl: &Table,
    key: &str,
    context: &str,
) -> LuaResult<Option<LocalizedString>> {
    match tbl.get::<Value>(key)? {
        Value::Nil => Ok(None),
        Value::String(s) => Ok(Some(LocalizedString::Plain(s.to_str()?.to_string()))),
        Value::Table(t) => {
            let mut map = HashMap::new();

            for pair in t.pairs::<Value, Value>() {
                let (k, v) = pair?;
                let (Value::String(k), Value::String(v)) = (&k, &v) else {
                    return Err(RuntimeError(format!(
                        "{context} '{key}' table entries must be locale = \"string\" pairs, \
                         got {} = {}",
                        k.type_name(),
                        v.type_name()
                    )));
                };
                map.insert(k.to_str()?.to_string(), v.to_str()?.to_string());
            }

            if map.is_empty() {
                return Err(RuntimeError(format!(
                    "{context} '{key}' table must not be empty \
                     (expected locale = \"string\" pairs)"
                )));
            }

            Ok(Some(LocalizedString::Localized(map)))
        }
        other => Err(RuntimeError(format!(
            "{context} '{key}' must be a string or a table of locale = \"string\" pairs, \
             got {}",
            other.type_name()
        ))),
    }
}

/// Strict optional sub-table: absent / `nil` → `None`, a table → `Some`,
/// any other present value → a hard error.
pub(super) fn get_optional_table(
    tbl: &Table,
    key: &str,
    context: &str,
) -> LuaResult<Option<Table>> {
    match tbl.get::<Value>(key)? {
        Value::Nil => Ok(None),
        Value::Table(t) => Ok(Some(t)),
        other => Err(RuntimeError(format!(
            "{context} '{key}' must be a table, got {}",
            other.type_name()
        ))),
    }
}

/// Strict string sequence: absent / `nil` → empty vec; a table → every entry
/// collected as a string; a non-table value or a non-string entry → a hard
/// error.
pub(super) fn get_string_sequence(tbl: &Table, key: &str, context: &str) -> LuaResult<Vec<String>> {
    let Some(seq) = get_optional_table(tbl, key, context)? else {
        return Ok(Vec::new());
    };

    seq.sequence_values::<String>()
        .collect::<LuaResult<Vec<_>>>()
        .map_err(|e| {
            RuntimeError(format!(
                "{context} '{key}' must be an array of strings: {e}"
            ))
        })
}

/// Read a boolean field from a Lua table.
///
/// - Missing key -> `default`.
/// - Boolean value -> returned as-is.
/// - Any other type -> error naming the key and the actual type, so typos
///   like `required = "true"` (string) surface at parse time instead of
///   silently falling back to the default.
pub(super) fn get_bool(tbl: &Table, key: &str, default: bool) -> mlua::Result<bool> {
    match tbl.get::<Value>(key)? {
        Value::Nil => Ok(default),
        Value::Boolean(b) => Ok(b),
        other => Err(mlua::Error::RuntimeError(format!(
            "Field config key '{}' expected a boolean, got {}",
            key,
            other.type_name()
        ))),
    }
}

/// Parse the `collection` field from a relationship Lua table.
///
/// The `collection` key may be:
/// - A plain string -> single-collection relationship, returns `(collection, vec![])`.
/// - A Lua array of strings -> polymorphic relationship, returns `(first, all_slugs)`.
///   `collection` is set to the first slug; `polymorphic` holds all slugs.
pub(super) fn parse_relationship_collection(
    rel_tbl: &Table,
    field_name: &str,
) -> mlua::Result<(String, Vec<String>)> {
    match rel_tbl.get::<Value>("collection")? {
        Value::String(s) => Ok((s.to_str()?.to_string(), vec![])),
        Value::Table(arr) => {
            // Every entry must be a string slug — a non-string entry used to
            // be silently SKIPPED, shrinking the polymorphic target set.
            let mut slugs = Vec::new();
            for v in arr.sequence_values::<Value>() {
                match v? {
                    Value::String(s) => slugs.push(s.to_str()?.to_string()),
                    other => {
                        return Err(mlua::Error::RuntimeError(format!(
                            "Field '{field_name}': relationship.collection array \
                             entries must be strings, got {}",
                            other.type_name()
                        )));
                    }
                }
            }
            let first = slugs.first().cloned().unwrap_or_default();

            Ok((first, slugs))
        }
        Value::Nil => Ok((String::new(), vec![])),
        other => Err(mlua::Error::RuntimeError(format!(
            "Field '{field_name}': relationship.collection must be a string or \
             an array of strings, got {}",
            other.type_name()
        ))),
    }
}

pub(super) fn get_string_val(tbl: &Table, key: &str) -> mlua::Result<String> {
    tbl.get(key)
}

/// Parse a single hook reference: a bare string `"hooks.posts.slugify"` or a
/// `{ ref = "hooks.posts.slugify", options = { ... } }` table.
///
/// The table form carries per-config `options` exposed to the hook as
/// `ctx.options`. Unknown keys are rejected (parity with the rest of the strict
/// config surface); `ref` is required and must be a string; `options`, when
/// present, must be a table.
pub(super) fn parse_hook_ref(value: Value, context: &str) -> Result<HookRef> {
    match value {
        Value::String(s) => Ok(HookRef::new(s.to_str()?.to_string())),
        Value::Table(t) => {
            deny_unknown_keys(&t, context, &["ref", "options"])?;

            let reference = get_optional_string_ref(&t, "ref", context)?
                .ok_or_else(|| anyhow::anyhow!("{context} table requires a string 'ref' key"))?;

            let options = match t.get::<Value>("options")? {
                Value::Nil => None,
                v @ Value::Table(_) => Some(lua_to_json(&v)?),
                other => bail!(
                    "{context} 'options' must be a table, got {}",
                    other.type_name()
                ),
            };

            Ok(HookRef { reference, options })
        }
        other => bail!(
            "{context} must be a string hook ref or a {{ ref, options }} table, got {}",
            other.type_name()
        ),
    }
}

/// Parse an array of hook references (each a string or `{ ref, options }` table).
/// Missing / non-table value at `key` yields an empty list.
pub(super) fn parse_hook_ref_list(tbl: &Table, key: &str) -> Result<Vec<HookRef>> {
    let Ok(list_tbl) = get_table(tbl, key) else {
        return Ok(Vec::new());
    };

    let mut items = Vec::new();

    for pair in list_tbl.sequence_values::<Value>() {
        items.push(parse_hook_ref(pair?, key)?);
    }

    Ok(items)
}

/// Strict optional single hook ref: absent / `nil` → `None`, a string or
/// `{ ref, options }` table → `Some`, any other present value → a hard error.
///
/// The single-ref analogue of [`get_optional_string_ref`], for sites like
/// access rules and field conditions that take one ref rather than a list.
pub(super) fn get_optional_hook_ref(
    tbl: &Table,
    key: &str,
    context: &str,
) -> Result<Option<HookRef>> {
    match tbl.get::<Value>(key)? {
        Value::Nil => Ok(None),
        // Fold the key into the context so a bad value names the offending rule
        // (e.g. `access 'read'`), not just the surface.
        other => Ok(Some(parse_hook_ref(other, &format!("{context} '{key}'"))?)),
    }
}

pub(super) fn parse_hooks(hooks_tbl: &Table) -> Result<Hooks> {
    Ok(Hooks::builder()
        .before_validate(parse_hook_ref_list(hooks_tbl, "before_validate")?)
        .before_change(parse_hook_ref_list(hooks_tbl, "before_change")?)
        .after_change(parse_hook_ref_list(hooks_tbl, "after_change")?)
        .before_read(parse_hook_ref_list(hooks_tbl, "before_read")?)
        .after_read(parse_hook_ref_list(hooks_tbl, "after_read")?)
        .before_delete(parse_hook_ref_list(hooks_tbl, "before_delete")?)
        .after_delete(parse_hook_ref_list(hooks_tbl, "after_delete")?)
        .before_broadcast(parse_hook_ref_list(hooks_tbl, "before_broadcast")?)
        .build())
}

pub(super) fn parse_select_options(opts_tbl: &Table) -> Result<Vec<SelectOption>> {
    let mut options = Vec::new();

    for pair in opts_tbl.clone().sequence_values::<Table>() {
        let opt = pair?;
        deny_unknown_keys(&opt, "select option", &["label", "value"])?;
        let label = get_localized_string(&opt, "label")
            .unwrap_or_else(|| LocalizedString::Plain(String::new()));
        let value = get_string_val(&opt, "value").unwrap_or_default();

        options.push(SelectOption::new(label, value));
    }

    Ok(options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::LocalizedString;
    use mlua::Lua;

    #[test]
    fn test_get_string_present() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("name", "hello").unwrap();
        assert_eq!(get_string(&tbl, "name"), Some("hello".to_string()));
    }

    #[test]
    fn test_get_string_absent() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        assert_eq!(get_string(&tbl, "name"), None);
    }

    #[test]
    fn test_get_string_non_string_value() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("num", 42).unwrap();
        assert_eq!(get_string(&tbl, "num"), Some("42".to_string()));
        let inner = lua.create_table().unwrap();
        tbl.set("tbl", inner).unwrap();
        assert_eq!(get_string(&tbl, "tbl"), None);
    }

    #[test]
    fn test_get_bool_present() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("active", true).unwrap();
        assert!(get_bool(&tbl, "active", false).unwrap());
    }

    #[test]
    fn test_get_bool_absent_default_true() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        assert!(get_bool(&tbl, "active", true).unwrap());
    }

    #[test]
    fn test_get_bool_absent_default_false() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        assert!(!get_bool(&tbl, "active", false).unwrap());
    }

    /// BUG-1 regression: a non-bool value must raise an error naming the key
    /// and the actual type, instead of silently using the default.
    #[test]
    fn test_get_bool_rejects_string_value() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("active", "true").unwrap();
        let err = get_bool(&tbl, "active", false).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("active"), "error should name the key: {msg}");
        assert!(
            msg.contains("boolean"),
            "error should mention boolean: {msg}"
        );
    }

    #[test]
    fn test_get_bool_rejects_number_value() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("active", 1i64).unwrap();
        assert!(get_bool(&tbl, "active", false).is_err());
    }

    #[test]
    fn test_get_bool_missing_uses_default() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        assert!(get_bool(&tbl, "missing", true).unwrap());
        assert!(!get_bool(&tbl, "missing", false).unwrap());
    }

    #[test]
    fn test_get_bool_present_true_returns_true() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("flag", true).unwrap();
        assert!(get_bool(&tbl, "flag", false).unwrap());
    }

    #[test]
    fn test_get_string_val_present() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("key", "value").unwrap();
        assert_eq!(get_string_val(&tbl, "key").unwrap(), "value");
    }

    #[test]
    fn test_get_string_val_absent() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        assert!(get_string_val(&tbl, "key").is_err());
    }

    #[test]
    fn test_get_table_present() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let inner = lua.create_table().unwrap();
        inner.set("foo", "bar").unwrap();
        tbl.set("inner", inner).unwrap();
        let result = get_table(&tbl, "inner");
        assert!(result.is_ok());
    }

    #[test]
    fn test_get_table_absent() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        assert!(get_table(&tbl, "inner").is_err());
    }

    #[test]
    fn test_get_localized_string_plain() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("label", "Hello").unwrap();
        let result = get_localized_string(&tbl, "label");
        match result {
            Some(LocalizedString::Plain(s)) => assert_eq!(s, "Hello"),
            other => panic!("Expected Plain, got {other:?}"),
        }
    }

    #[test]
    fn test_get_localized_string_localized() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let locale_tbl = lua.create_table().unwrap();
        locale_tbl.set("en", "Hello").unwrap();
        locale_tbl.set("de", "Hallo").unwrap();
        tbl.set("label", locale_tbl).unwrap();
        let result = get_localized_string(&tbl, "label");
        match result {
            Some(LocalizedString::Localized(map)) => {
                assert_eq!(map.get("en").unwrap(), "Hello");
                assert_eq!(map.get("de").unwrap(), "Hallo");
            }
            other => panic!("Expected Localized, got {other:?}"),
        }
    }

    #[test]
    fn test_get_localized_string_absent() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        assert!(get_localized_string(&tbl, "label").is_none());
    }

    #[test]
    fn test_get_localized_string_empty_table() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let empty = lua.create_table().unwrap();
        tbl.set("label", empty).unwrap();
        assert!(get_localized_string(&tbl, "label").is_none());
    }

    #[test]
    fn test_parse_relationship_collection_missing() {
        let lua = Lua::new();
        let rel_tbl = lua.create_table().unwrap();
        let (col, poly) = parse_relationship_collection(&rel_tbl, "f").unwrap();
        assert_eq!(col, "");
        assert!(poly.is_empty());
    }

    #[test]
    fn test_parse_relationship_collection_array() {
        let lua = Lua::new();
        let rel_tbl = lua.create_table().unwrap();
        let arr = lua.create_table().unwrap();
        arr.set(1, "posts").unwrap();
        arr.set(2, "pages").unwrap();
        rel_tbl.set("collection", arr).unwrap();
        let (col, poly) = parse_relationship_collection(&rel_tbl, "f").unwrap();
        assert_eq!(col, "posts");
        assert_eq!(poly, vec!["posts", "pages"]);
    }

    #[test]
    fn parse_hook_ref_bare_string() {
        let r = parse_hook_ref(
            Value::String(Lua::new().create_string("hooks.x").unwrap()),
            "bc",
        )
        .unwrap();
        assert_eq!(r, HookRef::new("hooks.x"));
    }

    #[test]
    fn parse_hook_ref_table_with_options() {
        let lua = Lua::new();
        let t = lua.create_table().unwrap();
        t.set("ref", "hooks.slugify").unwrap();
        let opts = lua.create_table().unwrap();
        opts.set("from", "title").unwrap();
        t.set("options", opts).unwrap();

        let r = parse_hook_ref(Value::Table(t), "before_change").unwrap();
        assert_eq!(r.reference, "hooks.slugify");
        assert_eq!(r.options.unwrap()["from"], serde_json::json!("title"));
    }

    #[test]
    fn parse_hook_ref_table_missing_ref_errors() {
        let lua = Lua::new();
        let t = lua.create_table().unwrap();
        let opts = lua.create_table().unwrap();
        t.set("options", opts).unwrap();

        let err = parse_hook_ref(Value::Table(t), "before_change").unwrap_err();
        assert!(err.to_string().contains("ref"), "{err}");
    }

    #[test]
    fn parse_hook_ref_table_unknown_key_rejected() {
        let lua = Lua::new();
        let t = lua.create_table().unwrap();
        t.set("ref", "hooks.x").unwrap();
        t.set("optionz", lua.create_table().unwrap()).unwrap();

        let err = parse_hook_ref(Value::Table(t), "before_change").unwrap_err();
        assert!(err.to_string().contains("optionz"), "{err}");
    }

    #[test]
    fn parse_hook_ref_options_must_be_table() {
        let lua = Lua::new();
        let t = lua.create_table().unwrap();
        t.set("ref", "hooks.x").unwrap();
        t.set("options", "nope").unwrap();

        let err = parse_hook_ref(Value::Table(t), "before_change").unwrap_err();
        assert!(err.to_string().contains("options"), "{err}");
    }

    #[test]
    fn parse_hook_ref_list_mixes_string_and_table() {
        let lua = Lua::new();
        let list = lua.create_table().unwrap();
        list.set(1, "hooks.bare").unwrap();
        let entry = lua.create_table().unwrap();
        entry.set("ref", "hooks.param").unwrap();
        let opts = lua.create_table().unwrap();
        opts.set("n", 3).unwrap();
        entry.set("options", opts).unwrap();
        list.set(2, entry).unwrap();

        let outer = lua.create_table().unwrap();
        outer.set("before_change", list).unwrap();

        let refs = parse_hook_ref_list(&outer, "before_change").unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0], HookRef::new("hooks.bare"));
        assert_eq!(refs[1].reference, "hooks.param");
        assert!(refs[1].options.is_some());
    }

    #[test]
    fn get_optional_hook_ref_nil_is_none() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        assert!(
            get_optional_hook_ref(&tbl, "read", "access")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn test_parse_relationship_collection_array_empty() {
        let lua = Lua::new();
        let rel_tbl = lua.create_table().unwrap();
        let arr = lua.create_table().unwrap();
        rel_tbl.set("collection", arr).unwrap();
        let (col, poly) = parse_relationship_collection(&rel_tbl, "f").unwrap();
        assert_eq!(col, "");
        assert!(poly.is_empty());
    }
}
