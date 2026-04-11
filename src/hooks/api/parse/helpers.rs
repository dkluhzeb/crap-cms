//! Shared helper functions for Lua table parsing.

use std::collections::HashMap;

use anyhow::Result;
use mlua::{Table, Value};

use crate::core::{LocalizedString, SelectOption, collection::Hooks};

pub(super) fn get_table(tbl: &Table, key: &str) -> mlua::Result<Table> {
    tbl.get(key)
}

pub(super) fn get_string(tbl: &Table, key: &str) -> Option<String> {
    tbl.get::<Option<String>>(key).ok().flatten()
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

pub(super) fn get_bool(tbl: &Table, key: &str, default: bool) -> bool {
    tbl.get::<Option<bool>>(key)
        .ok()
        .flatten()
        .unwrap_or(default)
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
                .filter_map(|r| r.ok())
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
    use crate::core::field::LocalizedString;
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
        assert!(get_bool(&tbl, "active", false));
    }

    #[test]
    fn test_get_bool_absent_default_true() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        assert!(get_bool(&tbl, "active", true));
    }

    #[test]
    fn test_get_bool_absent_default_false() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        assert!(!get_bool(&tbl, "active", false));
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
            other => panic!("Expected Plain, got {:?}", other),
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
            other => panic!("Expected Localized, got {:?}", other),
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
