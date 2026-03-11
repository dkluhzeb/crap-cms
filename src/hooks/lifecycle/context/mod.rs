//! Hook context types and Rust↔Lua marshalling.

mod builder;

pub use builder::HookContextBuilder;

use mlua::{Lua, Value};
use std::collections::HashMap;

use crate::core::field::{FieldDefinition, FieldType};
use crate::core::Document;

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
    /// Authenticated user document, if any. Exposed as `ctx.user` in Lua hooks.
    pub user: Option<Document>,
    /// Admin UI locale (e.g. "en", "de"). Exposed as `ctx.ui_locale` in Lua hooks.
    pub ui_locale: Option<String>,
}

impl HookContext {
    /// Create a builder with the required `collection` and `operation` fields.
    pub fn builder(
        collection: impl Into<String>,
        operation: impl Into<String>,
    ) -> HookContextBuilder {
        HookContextBuilder::new(collection.into(), operation.into())
    }

    /// Convert this context to a Lua table for passing to hook functions.
    pub(crate) fn to_lua_table(&self, lua: &Lua) -> mlua::Result<mlua::Table> {
        let ctx_table = lua.create_table()?;
        ctx_table.set("collection", self.collection.as_str())?;
        ctx_table.set("operation", self.operation.as_str())?;
        if let Some(ref locale) = self.locale {
            ctx_table.set("locale", locale.as_str())?;
        }
        if let Some(draft) = self.draft {
            ctx_table.set("draft", draft)?;
        }
        let data_table = lua.create_table()?;
        for (k, v) in &self.data {
            data_table.set(k.as_str(), crate::hooks::api::json_to_lua(lua, v)?)?;
        }
        ctx_table.set("data", data_table)?;

        // Request-scoped shared context table
        let context_table = lua.create_table()?;
        for (k, v) in &self.context {
            context_table.set(k.as_str(), crate::hooks::api::json_to_lua(lua, v)?)?;
        }
        ctx_table.set("context", context_table)?;

        // Expose current hook depth so hooks can make manual decisions
        let depth = lua.app_data_ref::<HookDepth>().map(|d| d.0).unwrap_or(0);
        ctx_table.set("hook_depth", depth)?;

        // Authenticated user document
        if let Some(ref user_doc) = self.user {
            let user_tbl =
                crate::hooks::lifecycle::converters::document_to_lua_table(lua, user_doc)?;
            ctx_table.set("user", user_tbl)?;
        }

        // Admin UI locale
        if let Some(ref ui_locale) = self.ui_locale {
            ctx_table.set("ui_locale", ui_locale.as_str())?;
        }

        Ok(ctx_table)
    }

    /// Convert data to a string map for query functions.
    ///
    /// Only includes fields that have parent table columns (skips array/has-many).
    /// Group fields are flattened from `{ "seo": { "meta_title": "X" } }` to
    /// `{ "seo__meta_title": "X" }` so `query::create/update` can find them.
    pub fn to_string_map(&self, fields: &[FieldDefinition]) -> HashMap<String, String> {
        let mut map = HashMap::new();
        for (k, v) in &self.data {
            // Check if this key is a group field that needs flattening
            let is_group = fields
                .iter()
                .any(|f| f.name == *k && f.field_type == FieldType::Group);
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
            map.insert(
                k.clone(),
                match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                },
            );
        }
        map
    }

    /// Read the `context` table from a returned Lua hook table, replacing `self.context`.
    pub(crate) fn read_context_back(&mut self, lua: &Lua, tbl: &mlua::Table) {
        if let Ok(context_tbl) = tbl.get::<mlua::Table>("context") {
            self.context.clear();
            for (k, v) in context_tbl.pairs::<String, Value>().flatten() {
                if let Ok(json_val) = crate::hooks::api::lua_to_json(lua, &v) {
                    self.context.insert(k, json_val);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn to_lua_table_with_locale_and_draft() {
        let lua = mlua::Lua::new();
        lua.set_app_data(HookDepth(3));
        let mut data = HashMap::new();
        data.insert("title".to_string(), json!("Hello"));
        let mut ctx_map = HashMap::new();
        ctx_map.insert("request_id".to_string(), json!("abc-123"));

        let ctx = HookContext::builder("posts", "create")
            .data(data)
            .locale("en")
            .draft(true)
            .context(ctx_map)
            .build();

        let tbl = ctx.to_lua_table(&lua).unwrap();
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

    #[test]
    fn read_context_back_replaces_existing() {
        let lua = mlua::Lua::new();
        let tbl = lua.create_table().unwrap();
        let context_tbl = lua.create_table().unwrap();
        context_tbl.set("key1", "value1").unwrap();
        context_tbl.set("key2", 42).unwrap();
        tbl.set("context", context_tbl).unwrap();

        let mut ctx_map = HashMap::new();
        ctx_map.insert("old_key".to_string(), json!("old_value"));
        let mut ctx = HookContext::builder("test", "create")
            .context(ctx_map)
            .build();
        ctx.read_context_back(&lua, &tbl);

        assert!(
            !ctx.context.contains_key("old_key"),
            "old entries should be cleared"
        );
        assert_eq!(ctx.context.get("key1"), Some(&json!("value1")));
        assert_eq!(ctx.context.get("key2"), Some(&json!(42)));
    }

    #[test]
    fn read_context_back_no_context_table() {
        let lua = mlua::Lua::new();
        let tbl = lua.create_table().unwrap();

        let mut ctx_map = HashMap::new();
        ctx_map.insert("old_key".to_string(), json!("old_value"));
        let mut ctx = HookContext::builder("test", "create")
            .context(ctx_map)
            .build();
        ctx.read_context_back(&lua, &tbl);

        assert!(ctx.context.contains_key("old_key"));
    }

    #[test]
    fn string_map_simple() {
        let mut data = HashMap::new();
        data.insert("title".to_string(), json!("Hello World"));
        data.insert("count".to_string(), json!(42));
        data.insert("active".to_string(), json!(true));

        let ctx = HookContext::builder("posts", "create").data(data).build();

        let fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("count", FieldType::Number).build(),
            FieldDefinition::builder("active", FieldType::Checkbox).build(),
        ];

        let map = ctx.to_string_map(&fields);
        assert_eq!(map.get("title").unwrap(), "Hello World");
        assert_eq!(map.get("count").unwrap(), "42");
        assert_eq!(map.get("active").unwrap(), "true");
    }

    #[test]
    fn string_map_group_flattening() {
        let mut data = HashMap::new();
        data.insert(
            "seo".to_string(),
            json!({
                "meta_title": "My Title",
                "meta_description": "My Description"
            }),
        );
        data.insert("title".to_string(), json!("Hello"));

        let ctx = HookContext::builder("posts", "create").data(data).build();

        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group).build(),
            FieldDefinition::builder("title", FieldType::Text).build(),
        ];

        let map = ctx.to_string_map(&fields);
        assert_eq!(map.get("seo__meta_title").unwrap(), "My Title");
        assert_eq!(map.get("seo__meta_description").unwrap(), "My Description");
        assert_eq!(map.get("title").unwrap(), "Hello");
        assert!(!map.contains_key("seo"));
    }

    #[test]
    fn string_map_group_non_object_value() {
        let mut data = HashMap::new();
        data.insert("seo".to_string(), json!("plain-string"));

        let ctx = HookContext::builder("posts", "create").data(data).build();

        let fields = vec![FieldDefinition::builder("seo", FieldType::Group).build()];

        let map = ctx.to_string_map(&fields);
        assert_eq!(map.get("seo").unwrap(), "plain-string");
    }

    #[test]
    fn string_map_group_with_numeric_subfields() {
        let mut data = HashMap::new();
        data.insert(
            "metrics".to_string(),
            json!({
                "views": 100,
                "likes": 42
            }),
        );

        let ctx = HookContext::builder("posts", "create").data(data).build();

        let fields = vec![FieldDefinition::builder("metrics", FieldType::Group).build()];

        let map = ctx.to_string_map(&fields);
        assert_eq!(map.get("metrics__views").unwrap(), "100");
        assert_eq!(map.get("metrics__likes").unwrap(), "42");
    }
}
