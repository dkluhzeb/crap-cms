//! Hook context types and Rust↔Lua marshalling.

use mlua::{Lua, Value};
use std::collections::HashMap;

use crate::core::field::{FieldDefinition, FieldType};

use super::HookDepth;

/// Context passed to hook functions.
#[derive(Debug, Clone)]
pub struct HookContext {
    pub collection: String,
    pub operation: String,
    pub data: HashMap<String, serde_json::Value>,
    pub locale: Option<String>,
    /// Whether this operation is a draft save (`true` = draft, `false`/`None` = publish).
    pub draft: Option<bool>,
    /// Request-scoped shared table that flows from before_validate through after_change.
    /// Hooks can read/write this to share state within one request lifecycle.
    /// Only JSON-compatible values survive (no functions, userdata, etc.).
    pub context: HashMap<String, serde_json::Value>,
}

/// Build a Lua table from a HookContext (shared by all context table builders).
pub(super) fn context_to_lua_table(lua: &Lua, context: &HookContext) -> mlua::Result<mlua::Table> {
    let ctx_table = lua.create_table()?;
    ctx_table.set("collection", context.collection.as_str())?;
    ctx_table.set("operation", context.operation.as_str())?;
    if let Some(ref locale) = context.locale {
        ctx_table.set("locale", locale.as_str())?;
    }
    if let Some(draft) = context.draft {
        ctx_table.set("draft", draft)?;
    }
    let data_table = lua.create_table()?;
    for (k, v) in &context.data {
        data_table.set(k.as_str(), crate::hooks::api::json_to_lua(lua, v)?)?;
    }
    ctx_table.set("data", data_table)?;

    // Request-scoped shared context table
    let context_table = lua.create_table()?;
    for (k, v) in &context.context {
        context_table.set(k.as_str(), crate::hooks::api::json_to_lua(lua, v)?)?;
    }
    ctx_table.set("context", context_table)?;

    // Expose current hook depth so hooks can make manual decisions
    let depth = lua.app_data_ref::<HookDepth>().map(|d| d.0).unwrap_or(0);
    ctx_table.set("hook_depth", depth)?;

    Ok(ctx_table)
}

/// Read the `context` table from a returned Lua hook table, merging into the existing context.
pub(super) fn read_context_back(lua: &Lua, tbl: &mlua::Table, existing: &mut HashMap<String, serde_json::Value>) {
    if let Ok(context_tbl) = tbl.get::<mlua::Table>("context") {
        existing.clear();
        for pair in context_tbl.pairs::<String, Value>() {
            if let Ok((k, v)) = pair {
                if let Ok(json_val) = crate::hooks::api::lua_to_json(lua, &v) {
                    existing.insert(k, json_val);
                }
            }
        }
    }
}

/// Convert hook context data (JSON values) back to string map for query functions.
/// Only includes fields that have parent table columns (skips array/has-many).
/// Group fields are flattened from `{ "seo": { "meta_title": "X" } }` to
/// `{ "seo__meta_title": "X" }` so `query::create/update` can find them.
pub fn hook_ctx_to_string_map(
    ctx: &HookContext,
    fields: &[FieldDefinition],
) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for (k, v) in &ctx.data {
        // Check if this key is a group field that needs flattening
        let is_group = fields.iter().any(|f| {
            f.name == *k && f.field_type == FieldType::Group
        });
        if is_group {
            if let Some(obj) = v.as_object() {
                for (sub_key, sub_val) in obj {
                    let flat_key = format!("{}__{}", k, sub_key);
                    let flat_val = match sub_val {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    map.insert(flat_key, flat_val);
                }
                continue;
            }
            // If the value is already a string (e.g. from form data), fall through
        }
        map.insert(k.clone(), match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        });
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- context_to_lua_table tests ---

    #[test]
    fn test_context_to_lua_table_with_locale_and_draft() {
        let lua = mlua::Lua::new();
        lua.set_app_data(HookDepth(3));
        let mut ctx_map = HashMap::new();
        ctx_map.insert("request_id".to_string(), json!("abc-123"));
        let ctx = HookContext {
            collection: "posts".to_string(),
            operation: "create".to_string(),
            data: {
                let mut d = HashMap::new();
                d.insert("title".to_string(), json!("Hello"));
                d
            },
            locale: Some("en".to_string()),
            draft: Some(true),
            context: ctx_map,
        };
        let tbl = context_to_lua_table(&lua, &ctx).unwrap();
        let collection: String = tbl.get("collection").unwrap();
        assert_eq!(collection, "posts");
        let locale: String = tbl.get("locale").unwrap();
        assert_eq!(locale, "en");
        let draft: bool = tbl.get("draft").unwrap();
        assert!(draft);
        let depth: u32 = tbl.get("hook_depth").unwrap();
        assert_eq!(depth, 3);
        let context_tbl: mlua::Table = tbl.get("context").unwrap();
        let req_id: String = context_tbl.get("request_id").unwrap();
        assert_eq!(req_id, "abc-123");
    }

    // --- read_context_back tests ---

    #[test]
    fn test_read_context_back() {
        let lua = mlua::Lua::new();
        let tbl = lua.create_table().unwrap();
        let context_tbl = lua.create_table().unwrap();
        context_tbl.set("key1", "value1").unwrap();
        context_tbl.set("key2", 42).unwrap();
        tbl.set("context", context_tbl).unwrap();

        let mut existing = HashMap::new();
        existing.insert("old_key".to_string(), json!("old_value"));
        read_context_back(&lua, &tbl, &mut existing);

        assert!(!existing.contains_key("old_key"), "old entries should be cleared");
        assert_eq!(existing.get("key1"), Some(&json!("value1")));
        assert_eq!(existing.get("key2"), Some(&json!(42)));
    }

    #[test]
    fn test_read_context_back_no_context_table() {
        let lua = mlua::Lua::new();
        let tbl = lua.create_table().unwrap();
        // No "context" key in the table

        let mut existing = HashMap::new();
        existing.insert("old_key".to_string(), json!("old_value"));
        read_context_back(&lua, &tbl, &mut existing);

        // Should not change existing since there is no context table
        assert!(existing.contains_key("old_key"));
    }

    // --- hook_ctx_to_string_map tests ---

    #[test]
    fn test_hook_ctx_to_string_map_simple() {
        let mut data = HashMap::new();
        data.insert("title".to_string(), json!("Hello World"));
        data.insert("count".to_string(), json!(42));
        data.insert("active".to_string(), json!(true));

        let ctx = HookContext {
            collection: "posts".to_string(),
            operation: "create".to_string(),
            data,
            locale: None,
            draft: None,
            context: HashMap::new(),
        };

        let fields = vec![
            FieldDefinition {
                name: "title".to_string(),
                field_type: FieldType::Text,
                ..Default::default()
            },
            FieldDefinition {
                name: "count".to_string(),
                field_type: FieldType::Number,
                ..Default::default()
            },
            FieldDefinition {
                name: "active".to_string(),
                field_type: FieldType::Checkbox,
                ..Default::default()
            },
        ];

        let map = hook_ctx_to_string_map(&ctx, &fields);
        assert_eq!(map.get("title").unwrap(), "Hello World");
        assert_eq!(map.get("count").unwrap(), "42");
        assert_eq!(map.get("active").unwrap(), "true");
    }

    #[test]
    fn test_hook_ctx_to_string_map_group_flattening() {
        let mut data = HashMap::new();
        data.insert("seo".to_string(), json!({
            "meta_title": "My Title",
            "meta_description": "My Description"
        }));
        data.insert("title".to_string(), json!("Hello"));

        let ctx = HookContext {
            collection: "posts".to_string(),
            operation: "create".to_string(),
            data,
            locale: None,
            draft: None,
            context: HashMap::new(),
        };

        let fields = vec![
            FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                ..Default::default()
            },
            FieldDefinition {
                name: "title".to_string(),
                field_type: FieldType::Text,
                ..Default::default()
            },
        ];

        let map = hook_ctx_to_string_map(&ctx, &fields);
        assert_eq!(map.get("seo__meta_title").unwrap(), "My Title");
        assert_eq!(map.get("seo__meta_description").unwrap(), "My Description");
        assert_eq!(map.get("title").unwrap(), "Hello");
        // The group key itself should not be present
        assert!(!map.contains_key("seo"));
    }

    #[test]
    fn test_hook_ctx_to_string_map_group_non_object_value() {
        // If a group field has a string value (e.g. from form data), fall through
        let mut data = HashMap::new();
        data.insert("seo".to_string(), json!("plain-string"));

        let ctx = HookContext {
            collection: "posts".to_string(),
            operation: "create".to_string(),
            data,
            locale: None,
            draft: None,
            context: HashMap::new(),
        };

        let fields = vec![FieldDefinition {
            name: "seo".to_string(),
            field_type: FieldType::Group,
            ..Default::default()
        }];

        let map = hook_ctx_to_string_map(&ctx, &fields);
        // Falls through to the string conversion
        assert_eq!(map.get("seo").unwrap(), "plain-string");
    }

    #[test]
    fn test_hook_ctx_to_string_map_group_with_numeric_subfields() {
        let mut data = HashMap::new();
        data.insert("metrics".to_string(), json!({
            "views": 100,
            "likes": 42
        }));

        let ctx = HookContext {
            collection: "posts".to_string(),
            operation: "create".to_string(),
            data,
            locale: None,
            draft: None,
            context: HashMap::new(),
        };

        let fields = vec![FieldDefinition {
            name: "metrics".to_string(),
            field_type: FieldType::Group,
            ..Default::default()
        }];

        let map = hook_ctx_to_string_map(&ctx, &fields);
        assert_eq!(map.get("metrics__views").unwrap(), "100");
        assert_eq!(map.get("metrics__likes").unwrap(), "42");
    }
}
