//! Hook context types and Rust↔Lua marshalling.

use mlua::{Lua, Result as LuaResult, Table};

use crate::{
    core::{Document, DocumentFields, ReqContext, event::EventUser},
    hooks::lifecycle::{
        HookDepth,
        converters::{document_to_lua_table, lua_table_to_json_map, map_to_lua_table},
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
    extra_field = "hook_depth integer  Current recursion depth. `0` = top-level API/admin call, `1+` = from Lua CRUD inside hooks. Hooks are skipped when this reaches `hooks.max_depth` (default: `3`).",
    extra_field = "options? table  Per-config options from this hook ref's `{ ref, options }` table; `nil` when the hook was configured as a bare ref string."
)]
pub struct HookContext {
    /// Collection slug.
    pub collection: String,
    /// The operation being performed.
    #[lua(ty = "\"create\"|\"update\"|\"delete\"|\"find\"|\"find_by_id\"|\"get\"|\"init\"")]
    pub operation: String,
    /// Document data. For read hooks, contains document fields including
    /// `id` / timestamps. For `before_delete` / `after_delete` hooks,
    /// contains the deleted document's fields plus `id` (and `soft_delete`
    /// for a soft delete) — so a hook can inspect what is being removed; a
    /// hard delete leaves no row to re-fetch, so `after_delete` relies on
    /// this snapshot. In `after_change` hooks, `data.id` carries the new
    /// document ID.
    #[lua(ty = "table<string, any>")]
    pub data: DocumentFields,
    /// The content locale this operation targets (e.g. `"en"`, `"de"`) — the
    /// requested locale, or the default locale when none was given. Nil when
    /// localization is disabled (and on the locale-agnostic `before_delete` /
    /// `after_delete` hooks, which remove the whole row across all locales).
    /// Otherwise the same resolved value every hook surface sees (field hooks,
    /// validators, access functions).
    pub locale: Option<String>,
    /// `true` when this is a draft save (only set for collections with
    /// `versions.drafts` enabled).
    pub draft: Option<bool>,
    /// Operation-scoped shared table that persists from `before_validate`
    /// through `after_change` within one write operation (or `before_read`
    /// → `after_read` for one read) — NOT across the whole HTTP request.
    /// Only JSON-compatible values survive (no functions / userdata).
    #[lua(ty = "table<string, any>")]
    pub context: ReqContext,
    /// Authenticated user document (nil if unauthenticated or no auth
    /// collection).
    #[lua(ty = "table", optional)]
    pub user: Option<Document>,
    /// Admin UI locale code (e.g., `"en"`, `"de"`). Nil if not set or
    /// called from gRPC without locale context.
    pub ui_locale: Option<String>,
    /// The id of the document this event targets, exposed to Lua as `ctx.id`
    /// (matching the field-hook, validator, and access contexts). Populated across
    /// the write lifecycle — `update`/`delete` before- and after-hooks,
    /// `after_change` on create (the freshly assigned id; `nil` in create's
    /// before-hooks, where no row exists yet), `after_read`, and `"default"` for
    /// globals. Also set on live-broadcast hooks (`before_broadcast`). (The Rust
    /// field stays `document_id`; only the Lua-facing key is `id`.)
    #[lua(rename = "id", optional)]
    pub document_id: Option<String>,
    /// The user who caused a live-broadcast mutation. Set on `before_broadcast`;
    /// `nil` elsewhere or for anonymous changes. (Distinct from `user`, the
    /// caller of a request — broadcast fires post-commit with no request user.)
    #[lua(ty = "{ id: string, email: string }", optional)]
    pub edited_by: Option<EventUser>,
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
        tbl.set("data", map_to_lua_table(lua, self.data.as_map())?)?;
        tbl.set("context", map_to_lua_table(lua, self.context.as_map())?)?;

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
        if let Some(ref id) = self.document_id {
            tbl.set("id", id.as_str())?;
        }
        if let Some(ref u) = self.edited_by {
            let eu = lua.create_table()?;
            eu.set("id", u.id.as_str())?;
            eu.set("email", u.email.as_str())?;
            tbl.set("edited_by", eu)?;
        }

        Ok(tbl)
    }

    /// The (canonical **nested**) document data to hand to the persist layer.
    ///
    /// Nested group objects are the canonical in-memory shape; the `db/query`
    /// write edge owns the nested→flat `group__sub` column conversion (the mirror
    /// of read-side hydration). Typed values flow through unchanged so the DB
    /// coercion path can preserve precision.
    #[must_use]
    pub fn to_value_map(&self) -> DocumentFields {
        self.data.clone()
    }

    /// Read the `context` table from a returned Lua hook table, replacing
    /// `self.context`. Propagates a conversion failure rather than silently
    /// dropping the offending key — matching `read_hook_result` and
    /// `lua_table_to_auth_user`, which both `?` on `lua_to_json`.
    pub(crate) fn read_context_back(&mut self, tbl: &Table) -> LuaResult<()> {
        if let Ok(context_tbl) = tbl.get::<Table>("context") {
            self.context = lua_table_to_json_map(&context_tbl)?.into();
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

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
        ctx.read_context_back(&tbl).unwrap();

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
        ctx.read_context_back(&tbl).unwrap();

        assert!(ctx.context.contains_key("old_key"));
    }

    /// Regression: a context value that has no JSON representation (here a NaN
    /// number) must make `read_context_back` return `Err`, not silently drop
    /// the key. This matches `read_hook_result` / `lua_table_to_auth_user`,
    /// which both `?` on the same conversion.
    #[test]
    fn read_context_back_propagates_conversion_error() {
        let lua = mlua::Lua::new();
        let tbl = lua.create_table().unwrap();
        let ctx_tbl = lua.create_table().unwrap();
        ctx_tbl.set("bad", f64::NAN).unwrap();
        tbl.set("context", ctx_tbl).unwrap();

        let mut ctx = HookContext::builder("test", "create").build();

        assert!(ctx.read_context_back(&tbl).is_err());
    }

    #[test]
    fn to_value_map_passes_data_through_unchanged() {
        // `to_value_map` returns the canonical (nested) data as-is; the
        // nested→flat column conversion lives at the `db/query` write edge.
        // Group flattening itself is tested in `core::group_repr`.
        let mut data = HashMap::new();
        data.insert("title".to_string(), json!("Hello World"));
        data.insert("count".to_string(), json!(42));
        data.insert("seo".to_string(), json!({ "meta_title": "T" }));

        let ctx = HookContext::builder("posts", "create").data(data).build();

        let map = ctx.to_value_map();
        assert_eq!(map.get("title"), Some(&json!("Hello World")));
        assert_eq!(map.get("count"), Some(&json!(42)));
        // Nested group object is preserved, not flattened.
        assert_eq!(map.get("seo"), Some(&json!({ "meta_title": "T" })));
    }
}
