//! Pure Lua <-> Rust type conversion helpers (no DB access, no side effects).

use mlua::{Lua, Result as LuaResult, Table, Value};
use serde_json::Value as JsonValue;
use std::collections::HashMap;

use crate::{
    core::{Document, FieldDefinition, FieldType},
    db::query::helpers::prefixed_name,
    hooks::lua_api,
};

// ── Lua <-> Rust type conversion helpers ────────────────────────────────────

/// Convert a Lua data table to `HashMap`<String, Value>.
/// Preserves nested tables (blocks, arrays, has-many IDs) unlike `lua_table_to_hashmap`
/// which only handles scalars.
pub(crate) fn lua_table_to_json_map(tbl: &Table) -> LuaResult<HashMap<String, JsonValue>> {
    let mut map = HashMap::new();

    for pair in tbl.pairs::<String, Value>() {
        let (k, v) = pair?;

        if matches!(v, Value::Nil) {
            continue;
        }

        map.insert(k, lua_api::lua_to_json(&v)?);
    }

    Ok(map)
}

/// Convert a Lua data table to a `HashMap`<String, String> for create/update.
pub(crate) fn lua_table_to_hashmap(tbl: &Table) -> LuaResult<HashMap<String, String>> {
    let mut map = HashMap::new();

    for pair in tbl.pairs::<String, Value>() {
        let (k, v) = pair?;
        let s = match v {
            Value::String(s) => s.to_str()?.to_string(),
            Value::Integer(i) => i.to_string(),
            Value::Number(n) => n.to_string(),
            Value::Boolean(b) => b.to_string(),
            _ => continue,
        };

        map.insert(k, s);
    }

    Ok(map)
}

/// Flatten group fields from a Lua data table into the data map.
/// Converts `seo = { meta_title = "X" }` → `seo__meta_title = "X"`.
pub(crate) fn flatten_lua_groups(
    tbl: &Table,
    fields: &[FieldDefinition],
    data: &mut HashMap<String, String>,
) -> LuaResult<()> {
    for field in fields {
        if field.field_type != FieldType::Group {
            continue;
        }

        let Ok(sub_table) = tbl.get::<Table>(field.name.as_str()) else {
            continue;
        };

        for sub in &field.fields {
            let Ok(val) = sub_table.get::<Value>(sub.name.as_str()) else {
                continue;
            };

            let s = match val {
                Value::String(s) => s.to_str()?.to_string(),
                Value::Integer(i) => i.to_string(),
                Value::Number(n) => n.to_string(),
                _ => continue,
            };

            data.insert(prefixed_name(&field.name, &sub.name), s);
        }
    }

    Ok(())
}

/// Convert a Document to a Lua table.
pub(crate) fn document_to_lua_table(lua: &Lua, doc: &Document) -> LuaResult<Table> {
    let tbl = lua.create_table()?;

    tbl.set("id", &*doc.id)?;

    for (k, v) in &doc.fields {
        tbl.set(k.as_str(), lua_api::json_to_lua(lua, v)?)?;
    }

    if let Some(ref ts) = doc.created_at {
        tbl.set("created_at", ts.as_str())?;
    }

    if let Some(ref ts) = doc.updated_at {
        tbl.set("updated_at", ts.as_str())?;
    }

    Ok(tbl)
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal,
    clippy::used_underscore_binding
)]
mod tests {
    use super::*;
    use crate::core::document::DocumentBuilder;
    use crate::core::field::{FieldDefinition, FieldType};
    use mlua::Lua;
    use serde_json::json;

    // --- lua_table_to_hashmap tests ---

    #[test]
    fn test_hashmap_from_strings() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("title", "Hello").unwrap();
        tbl.set("slug", "hello").unwrap();
        let map = lua_table_to_hashmap(&tbl).unwrap();
        assert_eq!(map.get("title").unwrap(), "Hello");
        assert_eq!(map.get("slug").unwrap(), "hello");
    }

    #[test]
    fn test_hashmap_from_mixed_types() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("title", "Hello").unwrap();
        tbl.set("count", 42).unwrap();
        tbl.set("ratio", 3.15).unwrap();
        tbl.set("active", true).unwrap();
        let map = lua_table_to_hashmap(&tbl).unwrap();
        assert_eq!(map.get("title").unwrap(), "Hello");
        assert_eq!(map.get("count").unwrap(), "42");
        assert_eq!(map.get("ratio").unwrap(), "3.15");
        assert_eq!(map.get("active").unwrap(), "true");
    }

    #[test]
    fn test_hashmap_skips_nil() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("title", "Hello").unwrap();
        // Nil values are skipped
        let map = lua_table_to_hashmap(&tbl).unwrap();
        assert_eq!(map.len(), 1);
    }

    // --- flatten_lua_groups tests ---

    #[test]
    fn test_flatten_groups_basic() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let seo = lua.create_table().unwrap();
        seo.set("meta_title", "My Title").unwrap();
        seo.set("meta_description", "My Desc").unwrap();
        tbl.set("seo", seo).unwrap();
        tbl.set("title", "Hello").unwrap();

        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("meta_title", FieldType::Text).build(),
                    FieldDefinition::builder("meta_description", FieldType::Textarea).build(),
                ])
                .build(),
            FieldDefinition::builder("title", FieldType::Text).build(),
        ];

        let mut data = HashMap::new();
        flatten_lua_groups(&tbl, &fields, &mut data).unwrap();
        assert_eq!(data.get("seo__meta_title").unwrap(), "My Title");
        assert_eq!(data.get("seo__meta_description").unwrap(), "My Desc");
        // Non-group fields are not touched
        assert!(!data.contains_key("title"));
    }

    #[test]
    fn test_flatten_groups_missing_subtable() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        // No "seo" key at all

        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("meta_title", FieldType::Text).build(),
                ])
                .build(),
        ];

        let mut data = HashMap::new();
        flatten_lua_groups(&tbl, &fields, &mut data).unwrap();
        assert!(data.is_empty());
    }

    #[test]
    fn test_flatten_groups_numeric_values() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let metrics = lua.create_table().unwrap();
        metrics.set("views", 100).unwrap();
        metrics.set("rating", 4.5).unwrap();
        tbl.set("metrics", metrics).unwrap();

        let fields = vec![
            FieldDefinition::builder("metrics", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("views", FieldType::Number).build(),
                    FieldDefinition::builder("rating", FieldType::Number).build(),
                ])
                .build(),
        ];

        let mut data = HashMap::new();
        flatten_lua_groups(&tbl, &fields, &mut data).unwrap();
        assert_eq!(data.get("metrics__views").unwrap(), "100");
        assert_eq!(data.get("metrics__rating").unwrap(), "4.5");
    }

    // --- document_to_lua_table tests ---

    #[test]
    fn test_document_to_lua_basic() {
        let lua = Lua::new();
        let mut fields = HashMap::new();
        fields.insert("title".to_string(), json!("Hello"));
        fields.insert("count".to_string(), json!(42));

        let doc = DocumentBuilder::new("abc123")
            .fields(fields)
            .created_at(Some("2024-01-01T00:00:00Z"))
            .updated_at(Some("2024-01-02T00:00:00Z"))
            .build();

        let tbl = document_to_lua_table(&lua, &doc).unwrap();
        let id: String = tbl.get("id").unwrap();
        let title: String = tbl.get("title").unwrap();
        let count: i64 = tbl.get("count").unwrap();
        let created: String = tbl.get("created_at").unwrap();
        let updated: String = tbl.get("updated_at").unwrap();
        assert_eq!(id, "abc123");
        assert_eq!(title, "Hello");
        assert_eq!(count, 42);
        assert_eq!(created, "2024-01-01T00:00:00Z");
        assert_eq!(updated, "2024-01-02T00:00:00Z");
    }

    #[test]
    fn test_document_to_lua_no_timestamps() {
        let lua = Lua::new();
        let doc = DocumentBuilder::new("xyz").build();

        let tbl = document_to_lua_table(&lua, &doc).unwrap();
        let id: String = tbl.get("id").unwrap();
        assert_eq!(id, "xyz");
        // No timestamps set
        let created: Value = tbl.get("created_at").unwrap();
        assert!(matches!(created, Value::Nil));
    }

    // Filter parsing tests moved to
    // `hooks::lua_api::crud::filter` alongside the new typed
    // input types.

    // --- lua_table_to_json_map tests ---

    #[test]
    fn test_json_map_basic() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("title", "Hello").unwrap();
        tbl.set("count", 42).unwrap();
        tbl.set("active", true).unwrap();
        let map = lua_table_to_json_map(&tbl).unwrap();
        assert_eq!(map.get("title").unwrap(), &json!("Hello"));
        assert_eq!(map.get("count").unwrap(), &json!(42));
        assert_eq!(map.get("active").unwrap(), &json!(true));
    }

    #[test]
    fn test_json_map_skips_nil() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("title", "Hello").unwrap();
        // Setting a key to nil removes it from Lua table iteration
        let map = lua_table_to_json_map(&tbl).unwrap();
        assert_eq!(map.len(), 1);
    }
}
