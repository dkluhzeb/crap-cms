//! Hook context types and Rust↔Lua marshalling.

use mlua::{Lua, Result as LuaResult, Table, Value};
use serde_json::{Map as JsonMap, Value as JsonValue};
use std::collections::HashMap;

use crate::{
    core::{Document, DocumentFields, FieldDefinition, FieldType, ReqContext},
    hooks::{
        lifecycle::{HookDepth, converters::document_to_lua_table},
        lua_api,
    },
    typegen::lua::LuaAnnotation,
};

use super::HookContextBuilder;

/// Context passed to hook functions.
///
/// `data` is mutable in `before_*` hooks; read-only in `after_*` hooks.
/// `hook_depth` (not on the Rust struct, added at `to_lua_table` time
/// from `HookDepth` app-data) tracks recursion depth — `0` for a
/// top-level API call, `1+` from Lua CRUD invoked inside another hook.
//
// `LuaAnnotation` derive emits the Lua-facing `crap.HookContext` class
// in `types/crap.lua`. Field types are overridden where the Rust shape
// (`DocumentFields` / `ReqContext` / `Option<Document>`) differs from
// what the Lua user sees on the hook context table.
#[derive(Debug, Clone, LuaAnnotation)]
#[lua(
    class = "crap.HookContext",
    extra_field = "hook_depth integer  Current recursion depth. `0` = top-level API/admin call, `1+` = from Lua CRUD inside hooks. Hooks are skipped when this reaches `hooks.max_depth` (default: `3`)."
)]
pub struct HookContext {
    /// Collection slug.
    pub collection: String,
    /// The operation being performed.
    #[lua(ty = "\"create\"|\"update\"|\"delete\"|\"find\"|\"find_by_id\"|\"get_global\"|\"init\"")]
    pub operation: String,
    /// Document data. For read hooks, contains document fields including
    /// `id` / timestamps. For delete hooks, contains only
    /// `{ id = "..." }`. In `after_change` hooks, `data.id` carries the
    /// new document ID.
    #[lua(ty = "table<string, any>")]
    pub data: DocumentFields,
    /// Current locale code (nil if localization disabled or default
    /// locale).
    pub locale: Option<String>,
    /// `true` when this is a draft save (only set for collections with
    /// `versions.drafts` enabled).
    pub draft: Option<bool>,
    /// Request-scoped shared table that persists from `before_validate`
    /// through `after_change` within one request. Only JSON-compatible
    /// values survive (no functions / userdata).
    #[lua(ty = "table<string, any>")]
    pub context: ReqContext,
    /// Authenticated user document (nil if unauthenticated or no auth
    /// collection).
    #[lua(ty = "table", optional)]
    pub user: Option<Document>,
    /// Admin UI locale code (e.g., `"en"`, `"de"`). Nil if not set or
    /// called from gRPC without locale context.
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
    pub(crate) fn to_lua_table(&self, lua: &Lua) -> LuaResult<Table> {
        let tbl = lua.create_table()?;

        tbl.set("collection", self.collection.as_str())?;
        tbl.set("operation", self.operation.as_str())?;
        tbl.set("data", hashmap_to_lua(lua, &self.data)?)?;
        tbl.set("context", hashmap_to_lua(lua, &self.context)?)?;

        let depth = lua.app_data_ref::<HookDepth>().map_or(0, |d| d.0);
        tbl.set("hook_depth", depth)?;

        if let Some(ref v) = self.locale {
            tbl.set("locale", v.as_str())?;
        }
        if let Some(v) = self.draft {
            tbl.set("draft", v)?;
        }
        if let Some(ref v) = self.ui_locale {
            tbl.set("ui_locale", v.as_str())?;
        }
        if let Some(ref doc) = self.user {
            tbl.set("user", document_to_lua_table(lua, doc)?)?;
        }

        Ok(tbl)
    }

    /// Convert data to a typed-value map for `query::create`/`query::update`.
    ///
    /// Only includes fields that have parent table columns (skips array/has-many).
    /// Group fields are flattened from `{ "seo": { "meta_title": "X" } }` to
    /// `{ "seo__meta_title": "X" }` so `query::create/update` can find them.
    /// Typed values (Number, Bool, etc.) flow through unchanged so the DB
    /// coercion path can preserve precision via `coerce_json_value`.
    #[must_use]
    pub fn to_value_map(&self, fields: &[FieldDefinition]) -> DocumentFields {
        flatten_group_fields(&self.data, fields)
    }

    /// Read the `context` table from a returned Lua hook table, replacing `self.context`.
    pub(crate) fn read_context_back(&mut self, tbl: &Table) {
        if let Ok(context_tbl) = tbl.get::<Table>("context") {
            self.context.clear();

            for (k, v) in context_tbl.pairs::<String, Value>().flatten() {
                if let Ok(json_val) = lua_api::lua_to_json(&v) {
                    self.context.insert(k, json_val);
                }
            }
        }
    }
}

/// Convert a `HashMap`<String, `JsonValue`> to a Lua table.
fn hashmap_to_lua(lua: &Lua, map: &HashMap<String, JsonValue>) -> LuaResult<Table> {
    let tbl = lua.create_table()?;

    for (k, v) in map {
        tbl.set(k.as_str(), lua_api::json_to_lua(lua, v)?)?;
    }

    Ok(tbl)
}

/// Normalize a document data map so group fields are **flat** (`seo__meta_title`)
/// rather than nested (`{ seo: { meta_title } }`).
///
/// The Lua surface already flattens before reaching the service (so this is a
/// no-op there); the typed JSON surfaces (gRPC, MCP) and admin forms can arrive
/// nested. Applying this both before validation and at persist time keeps the
/// validator and the DB-write seeing the same shape.
///
/// A key is flattened only when it names a `Group` field whose value is an
/// object; otherwise it passes through untouched (so already-flat data is
/// unchanged, and non-group object values — e.g. a top-level `Json` field — are
/// preserved).
#[must_use]
pub(crate) fn flatten_group_fields(
    data: &DocumentFields,
    fields: &[FieldDefinition],
) -> DocumentFields {
    let mut map = DocumentFields::new();

    for (k, v) in data.as_map() {
        let is_group = fields
            .iter()
            .any(|f| f.name == *k && f.field_type == FieldType::Group);

        if is_group && let Some(obj) = v.as_object() {
            flatten_group_obj(k, obj, &mut map);

            continue;
        }

        map.insert(k.clone(), v.clone());
    }

    map
}

/// Recursively flatten a group object into `prefix__key` typed pairs.
fn flatten_group_obj(prefix: &str, obj: &JsonMap<String, JsonValue>, map: &mut DocumentFields) {
    for (sub_key, sub_val) in obj {
        let flat_key = format!("{prefix}__{sub_key}");

        if let JsonValue::Object(nested) = sub_val {
            flatten_group_obj(&flat_key, nested, map);
        } else {
            map.insert(flat_key, sub_val.clone());
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
        let mut ctx_map = ReqContext::new();
        ctx_map.insert("request_id".to_string(), json!("abc-123"));

        let ctx = HookContext::builder("posts", "create")
            .data(data)
            .locale(Some("en"))
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

        let mut ctx_map = ReqContext::new();
        ctx_map.insert("old_key".to_string(), json!("old_value"));
        let mut ctx = HookContext::builder("test", "create")
            .context(ctx_map)
            .build();
        ctx.read_context_back(&tbl);

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

        let mut ctx_map = ReqContext::new();
        ctx_map.insert("old_key".to_string(), json!("old_value"));
        let mut ctx = HookContext::builder("test", "create")
            .context(ctx_map)
            .build();
        ctx.read_context_back(&tbl);

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

        let map = ctx.to_value_map(&fields);
        // Typed values flow through unchanged so coerce_json_value can
        // preserve precision per field_type.
        assert_eq!(map.get("title"), Some(&json!("Hello World")));
        assert_eq!(map.get("count"), Some(&json!(42)));
        assert_eq!(map.get("active"), Some(&json!(true)));
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
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("meta_title", FieldType::Text).build(),
                    FieldDefinition::builder("meta_description", FieldType::Text).build(),
                ])
                .build(),
            FieldDefinition::builder("title", FieldType::Text).build(),
        ];

        let map = ctx.to_value_map(&fields);
        assert_eq!(map.get("seo__meta_title"), Some(&json!("My Title")));
        assert_eq!(
            map.get("seo__meta_description"),
            Some(&json!("My Description"))
        );
        assert_eq!(map.get("title"), Some(&json!("Hello")));
        assert!(!map.contains_key("seo"));
    }

    #[test]
    fn string_map_group_non_object_value() {
        let mut data = HashMap::new();
        data.insert("seo".to_string(), json!("plain-string"));

        let ctx = HookContext::builder("posts", "create").data(data).build();

        let fields = vec![FieldDefinition::builder("seo", FieldType::Group).build()];

        let map = ctx.to_value_map(&fields);
        assert_eq!(map.get("seo"), Some(&json!("plain-string")));
    }

    #[test]
    fn string_map_nested_group_flattening() {
        let mut data = HashMap::new();
        data.insert(
            "address".to_string(),
            json!({
                "geo": {
                    "lat": "40.7128",
                    "lng": "-74.0060"
                }
            }),
        );

        let ctx = HookContext::builder("companies", "create")
            .data(data)
            .build();

        let fields = vec![
            FieldDefinition::builder("address", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("geo", FieldType::Group)
                        .fields(vec![
                            FieldDefinition::builder("lat", FieldType::Text).build(),
                            FieldDefinition::builder("lng", FieldType::Text).build(),
                        ])
                        .build(),
                ])
                .build(),
        ];

        let map = ctx.to_value_map(&fields);
        assert_eq!(map.get("address__geo__lat"), Some(&json!("40.7128")));
        assert_eq!(map.get("address__geo__lng"), Some(&json!("-74.0060")));
        assert!(!map.contains_key("address"));
        assert!(!map.contains_key("address__geo"));
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

        let fields = vec![
            FieldDefinition::builder("metrics", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("views", FieldType::Number).build(),
                    FieldDefinition::builder("likes", FieldType::Number).build(),
                ])
                .build(),
        ];

        let map = ctx.to_value_map(&fields);
        assert_eq!(map.get("metrics__views"), Some(&json!(100)));
        assert_eq!(map.get("metrics__likes"), Some(&json!(42)));
    }
}
