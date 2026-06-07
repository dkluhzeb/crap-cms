//! Shared helper functions for Lua table parsing.

use std::collections::HashMap;

use anyhow::{Result, bail};
use mlua::{Table, Value};

use crate::core::{LocalizedString, SelectOption, collection::Hooks};

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
pub(super) fn parse_relationship_collection(rel_tbl: &Table) -> (String, Vec<String>) {
    match rel_tbl.get::<Value>("collection") {
        Ok(Value::String(s)) => {
            let col = s.to_str().ok().map(|v| v.to_string()).unwrap_or_default();
            (col, vec![])
        }
        Ok(Value::Table(arr)) => {
            let slugs: Vec<String> = arr
                .sequence_values::<String>()
                .filter_map(std::result::Result::ok)
                .collect();
            let first = slugs.first().cloned().unwrap_or_default();

            (first, slugs)
        }
        _ => (String::new(), vec![]),
    }
}

pub(super) fn get_string_val(tbl: &Table, key: &str) -> mlua::Result<String> {
    tbl.get(key)
}

pub(super) fn parse_string_list(tbl: &Table, key: &str) -> Result<Vec<String>> {
    if let Ok(list_tbl) = get_table(tbl, key) {
        let mut items = Vec::new();

        for pair in list_tbl.sequence_values::<String>() {
            items.push(pair?);
        }

        Ok(items)
    } else {
        Ok(Vec::new())
    }
}

pub(super) fn parse_hooks(hooks_tbl: &Table) -> Result<Hooks> {
    Ok(Hooks::builder()
        .before_validate(parse_string_list(hooks_tbl, "before_validate")?)
        .before_change(parse_string_list(hooks_tbl, "before_change")?)
        .after_change(parse_string_list(hooks_tbl, "after_change")?)
        .before_read(parse_string_list(hooks_tbl, "before_read")?)
        .after_read(parse_string_list(hooks_tbl, "after_read")?)
        .before_delete(parse_string_list(hooks_tbl, "before_delete")?)
        .after_delete(parse_string_list(hooks_tbl, "after_delete")?)
        .before_broadcast(parse_string_list(hooks_tbl, "before_broadcast")?)
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
        let (col, poly) = parse_relationship_collection(&rel_tbl);
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
        let (col, poly) = parse_relationship_collection(&rel_tbl);
        assert_eq!(col, "posts");
        assert_eq!(poly, vec!["posts", "pages"]);
    }

    #[test]
    fn test_parse_relationship_collection_array_empty() {
        let lua = Lua::new();
        let rel_tbl = lua.create_table().unwrap();
        let arr = lua.create_table().unwrap();
        rel_tbl.set("collection", arr).unwrap();
        let (col, poly) = parse_relationship_collection(&rel_tbl);
        assert_eq!(col, "");
        assert!(poly.is_empty());
    }
}
