//! Unpublish operation — reverts a published document to draft status.

use std::sync::Arc;

use mlua::{
    Error::RuntimeError, FromLua, Lua, LuaSerdeExt, Result as LuaResult, Table, Value as LuaValue,
};
use serde::Deserialize;
use serde_json::Value;

use anyhow::Result;

use crate::{
    core::{CollectionDefinition, Document, Registry},
    db::{DbConnection, query},
    hooks::{
        HookContext, HookEvent,
        lifecycle::{converters::document_to_lua_table, run_hooks_inner},
        lua_api::crud::{
            get_tx_conn,
            helpers::{
                check_hook_depth, hook_locale_config, hook_lua_infra, hook_ui_locale, hook_user,
                resolve_collection,
            },
        },
    },
    service::{LuaWriteHooks, ServiceContext, persist_unpublish, unpublish_document},
    typegen::lua::{LuaAnnotation, LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table},
};

/// Optional options for `crap.collections.unpublish`.
#[derive(Deserialize, LuaAnnotation)]
#[serde(default, deny_unknown_fields)]
#[lua(class = "crap.UnpublishOptions")]
pub(crate) struct UnpublishOptions {
    /// Skip access control checks (default: `false`).
    #[serde(rename = "overrideAccess")]
    #[lua(rename = "overrideAccess", optional)]
    pub(crate) override_access: bool,
    /// Run lifecycle hooks (default: `true`).
    #[lua(optional)]
    pub(crate) hooks: bool,
}

impl Default for UnpublishOptions {
    fn default() -> Self {
        Self {
            override_access: false,
            hooks: true,
        }
    }
}

impl FromLua for UnpublishOptions {
    fn from_lua(value: LuaValue, lua: &Lua) -> LuaResult<Self> {
        match value {
            LuaValue::Nil => Ok(Self::default()),
            other => lua.from_value(other),
        }
    }
}

/// Unpublish a document — sets `_status` to `"draft"` without modifying
/// the underlying field data. Only available on collections with
/// `versions` enabled.
/// Inside hooks, runs within the parent operation's transaction.
#[lua_fn(
    path = "crap.collections.unpublish",
    returns = "crap.Document",
    auto_tx
)]
fn collections_unpublish(
    state: &Arc<Registry>,
    lua: &Lua,
    #[lua(doc = "Collection slug.")] collection: String,
    #[lua(doc = "Document ID.")] id: String,
    #[lua(ty = "crap.UnpublishOptions", doc = "Optional options.")] opts: Option<UnpublishOptions>,
) -> LuaResult<Table> {
    let opts = opts.unwrap_or_default();
    let conn = get_tx_conn(lua)?;

    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let lua_infra = hook_lua_infra(lua);
    let def = resolve_collection(state, &collection)?;

    if !def.has_versions() {
        return Err(RuntimeError(format!(
            "Collection '{collection}' does not have versioning enabled"
        )));
    }

    let (hooks_enabled, _guard) = check_hook_depth(lua, opts.hooks, &collection, "update");

    let write_hooks = LuaWriteHooks::builder(lua)
        .user(user.as_ref())
        .ui_locale(ui_locale.as_deref())
        .override_access(opts.override_access)
        .registry(Some(state.as_ref()))
        .hooks_enabled(hooks_enabled)
        .build();

    let locale_config = hook_locale_config(lua);

    let ctx = ServiceContext::collection(&collection, &def)
        .conn(conn)
        .write_hooks(&write_hooks)
        .user(user.as_ref())
        .override_access(opts.override_access)
        .locale_config(locale_config.as_ref())
        .lua_infra(lua_infra.as_ref())
        .build();

    let doc = unpublish_document(&ctx, &id)
        .map_err(|e| RuntimeError(format!("unpublish error: {e:#}")))?;

    document_to_lua_table(lua, &doc)
}

lua_table! {
    name: crap_collections_unpublish,
    path: "crap.collections",
    state: Arc<Registry>,
    fns: [collections_unpublish],
}

/// Parameters for the unpublish operation.
pub(super) struct UnpublishCtx<'a> {
    pub(super) collection: &'a str,
    pub(super) id: &'a str,
    pub(super) def: &'a CollectionDefinition,
    pub(super) run_hooks: bool,
    pub(super) locale_str: Option<&'a str>,
    pub(super) hook_user: Option<&'a Document>,
    pub(super) hook_ui_locale: Option<&'a str>,
}

impl<'a> UnpublishCtx<'a> {
    /// Create a builder with the required fields.
    pub(super) fn builder(
        collection: &'a str,
        id: &'a str,
        def: &'a CollectionDefinition,
    ) -> UnpublishCtxBuilder<'a> {
        UnpublishCtxBuilder::new(collection, id, def)
    }
}

/// Handle the unpublish code path: revert to draft, fire hooks, return document.
pub(super) fn handle_unpublish(
    lua: &Lua,
    conn: &dyn DbConnection,
    ctx: &UnpublishCtx,
) -> mlua::Result<Table> {
    // Internal hook lifecycle lookup — fetches doc state before unpublish, not a user-facing read.
    let existing_doc = query::find_by_id_raw(conn, ctx.collection, ctx.def, ctx.id, None, false)
        .map_err(|e| RuntimeError(format!("find error: {e:#}")))?
        .ok_or_else(|| {
            RuntimeError(format!(
                "Document {} not found in {}",
                ctx.id, ctx.collection
            ))
        })?;

    let (hooks_enabled, _guard) = check_hook_depth(lua, ctx.run_hooks, ctx.collection, "update");

    if hooks_enabled {
        let before_ctx = HookContext::builder(ctx.collection, "update")
            .data(existing_doc.fields.clone())
            .draft(true)
            .locale(ctx.locale_str)
            .user(ctx.hook_user)
            .ui_locale(ctx.hook_ui_locale)
            .build();

        run_hooks_inner(lua, &ctx.def.hooks, HookEvent::BeforeChange, before_ctx)
            .map_err(|e| RuntimeError(format!("before_change hook error: {e:#}")))?;
    }

    let lua_infra = hook_lua_infra(lua);
    let locale_config = hook_locale_config(lua);

    let svc_ctx = ServiceContext::collection(ctx.collection, ctx.def)
        .conn(conn)
        .locale_config(locale_config.as_ref())
        .lua_infra(lua_infra.as_ref())
        .build();

    persist_unpublish(&svc_ctx, ctx.id)
        .map_err(|e| RuntimeError(format!("unpublish error: {e:#}")))?;

    // Internal hook lifecycle lookup — fetches doc state after unpublish, not
    // a user-facing read. Use the same locale-aware context as the unpublish
    // read so localized fields produce a SELECT that matches actual columns.
    let post_locale_ctx = svc_ctx.default_locale_ctx();

    let updated_doc = query::find_by_id_raw(
        conn,
        ctx.collection,
        ctx.def,
        ctx.id,
        post_locale_ctx.as_ref(),
        false,
    )
    .map_err(|e| RuntimeError(format!("find error after unpublish: {e:#}")))?
    .ok_or_else(|| {
        RuntimeError(format!(
            "Document {} not found after unpublish in {}",
            ctx.id, ctx.collection
        ))
    })?;

    if hooks_enabled {
        let mut after_data = updated_doc.fields.clone();
        after_data.insert("id".to_string(), Value::String(ctx.id.to_string()));

        let after_ctx = HookContext::builder(ctx.collection, "update")
            .data(after_data)
            .draft(true)
            .locale(ctx.locale_str)
            .user(ctx.hook_user)
            .ui_locale(ctx.hook_ui_locale)
            .build();

        run_hooks_inner(lua, &ctx.def.hooks, HookEvent::AfterChange, after_ctx)
            .map_err(|e| RuntimeError(format!("after_change hook error: {e:#}")))?;
    }

    document_to_lua_table(lua, &updated_doc)
}

/// Builder for [`UnpublishCtx`].
pub(super) struct UnpublishCtxBuilder<'a> {
    collection: &'a str,
    id: &'a str,
    def: &'a CollectionDefinition,
    run_hooks: bool,
    locale_str: Option<&'a str>,
    hook_user: Option<&'a Document>,
    hook_ui_locale: Option<&'a str>,
}

impl<'a> UnpublishCtxBuilder<'a> {
    pub(super) fn new(collection: &'a str, id: &'a str, def: &'a CollectionDefinition) -> Self {
        Self {
            collection,
            id,
            def,
            run_hooks: true,
            locale_str: None,
            hook_user: None,
            hook_ui_locale: None,
        }
    }

    pub(super) fn run_hooks(mut self, v: bool) -> Self {
        self.run_hooks = v;
        self
    }

    pub(super) fn locale_str(mut self, v: Option<&'a str>) -> Self {
        self.locale_str = v;
        self
    }

    pub(super) fn hook_user(mut self, v: Option<&'a Document>) -> Self {
        self.hook_user = v;
        self
    }

    pub(super) fn hook_ui_locale(mut self, v: Option<&'a str>) -> Self {
        self.hook_ui_locale = v;
        self
    }

    pub(super) fn build(self) -> UnpublishCtx<'a> {
        UnpublishCtx {
            collection: self.collection,
            id: self.id,
            def: self.def,
            run_hooks: self.run_hooks,
            locale_str: self.locale_str,
            hook_user: self.hook_user,
            hook_ui_locale: self.hook_ui_locale,
        }
    }
}

/// Register `crap.collections.unpublish(collection, id, opts?)`. Parent
/// `crap.collections` must already exist.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_unpublish(lua: &Lua, _table: &Table, registry: Arc<Registry>) -> Result<()> {
    register_crap_collections_unpublish(lua, registry)?;
    Ok(())
}
