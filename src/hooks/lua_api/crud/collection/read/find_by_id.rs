//! Registration of `crap.collections.find_by_id` Lua function.

use std::sync::Arc;

use anyhow::Result;
use mlua::{Error::RuntimeError, FromLua, Lua, LuaSerdeExt, Result as LuaResult, Table, Value};
use serde::Deserialize;

use crate::{
    config::LocaleConfig,
    core::Registry,
    db::LocaleContext,
    hooks::{
        lifecycle::converters::document_to_lua_table,
        lua_api::crud::{
            get_tx_conn,
            helpers::{hook_populate_singleflight, hook_ui_locale, hook_user, resolve_collection},
        },
    },
    service::{FindByIdInput, LuaReadHooks, ServiceContext, find_document_by_id},
    typegen::lua::{LuaAnnotation, LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table},
};

/// Optional options for `crap.collections.find_by_id`.
#[derive(Default, Deserialize, LuaAnnotation)]
#[serde(default)]
#[lua(class = "crap.FindByIdOptions")]
pub(crate) struct FindByIdOptions {
    /// Population depth for relationship fields (default: `0`). `0` =
    /// return IDs only. Clamped to the `[0, 10]` range.
    #[lua(optional)]
    pub(crate) depth: i32,
    /// Locale code for localized fields (e.g., `"en"`, `"de"`, `"all"`).
    /// Nil = default locale.
    pub(crate) locale: Option<String>,
    /// Fields to return. Nil or empty = all fields. `id` is always
    /// included.
    pub(crate) select: Option<Vec<String>>,
    /// When `true` and the collection has `versions.drafts`, returns
    /// the latest draft version snapshot instead of the published
    /// main-table data.
    #[lua(optional)]
    pub(crate) draft: bool,
    /// Skip access control checks (default: `false`). Set to `true` in
    /// trusted internal code to bypass collection-level and field-level
    /// access for the current user.
    #[serde(rename = "overrideAccess")]
    #[lua(rename = "overrideAccess", optional)]
    pub(crate) override_access: bool,
}

impl FromLua for FindByIdOptions {
    fn from_lua(value: Value, lua: &Lua) -> LuaResult<Self> {
        match value {
            Value::Nil => Ok(Self::default()),
            other => lua.from_value(other),
        }
    }
}

/// State threaded into `crap.collections.find_by_id`.
pub(crate) struct CollectionsFindByIdState {
    pub(crate) registry: Arc<Registry>,
    pub(crate) locale_config: LocaleConfig,
}

/// Find a single document by ID. Returns `nil` if not found.
/// Inside hooks, runs within the parent operation's transaction.
#[lua_fn(
    path = "crap.collections.find_by_id",
    returns = "crap.Document?",
    auto_tx
)]
fn collections_find_by_id(
    state: &CollectionsFindByIdState,
    lua: &Lua,
    #[lua(doc = "Collection slug.")] collection: String,
    #[lua(doc = "Document ID.")] id: String,
    #[lua(
        ty = "crap.FindByIdOptions",
        doc = "Optional options (e.g., `{ depth = 1 }`)."
    )]
    opts: Option<FindByIdOptions>,
) -> LuaResult<Value> {
    let opts = opts.unwrap_or_default();
    let reg = &state.registry;
    let lc = &state.locale_config;

    let conn = get_tx_conn(lua)?;

    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let depth = opts.depth.clamp(0, 10);
    let locale_ctx = LocaleContext::from_locale_string(opts.locale.as_deref(), lc)
        .map_err(|e| RuntimeError(e.to_string()))?;
    let def = resolve_collection(reg, &collection)?;

    let hooks = LuaReadHooks::builder(lua)
        .user(user.as_ref())
        .ui_locale(ui_locale.as_deref())
        .override_access(opts.override_access)
        .build();

    let ctx = ServiceContext::collection(&collection, &def)
        .conn(conn)
        .read_hooks(&hooks)
        .user(user.as_ref())
        .override_access(opts.override_access)
        .build();

    let input = FindByIdInput::builder(&id)
        .depth(depth)
        .locale_ctx(locale_ctx.as_ref())
        .registry(Some(reg.as_ref()))
        .select(opts.select.as_deref())
        .use_draft(opts.draft)
        .singleflight(hook_populate_singleflight(lua))
        .build();

    let doc = find_document_by_id(&ctx, &input).map_err(|e| RuntimeError(format!("{e}")))?;

    match doc {
        Some(d) => Ok(Value::Table(document_to_lua_table(lua, &d)?)),
        None => Ok(Value::Nil),
    }
}

lua_table! {
    name: crap_collections_find_by_id,
    path: "crap.collections",
    state: CollectionsFindByIdState,
    fns: [collections_find_by_id],
}

/// Register `crap.collections.find_by_id(collection, id, opts?)`. Parent
/// `crap.collections` must already exist.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_find_by_id(
    lua: &Lua,
    _table: &Table,
    registry: Arc<Registry>,
    locale_config: &LocaleConfig,
) -> Result<()> {
    register_crap_collections_find_by_id(
        lua,
        CollectionsFindByIdState {
            registry,
            locale_config: locale_config.clone(),
        },
    )?;
    Ok(())
}
