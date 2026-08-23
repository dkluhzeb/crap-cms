//! Registration of `crap.collections.count` Lua function.

use std::collections::HashMap;
use std::sync::Arc;

use crate::hooks::lua_api::utils::lua_err;
use anyhow::Result;
use mlua::{FromLua, Lua, LuaSerdeExt, Result as LuaResult, Table, Value};
use serde::Deserialize;

use crate::{
    config::LocaleConfig,
    core::Registry,
    db::{FindQuery, LocaleContext, query::filter::normalize_filter_fields},
    hooks::lua_api::crud::{
        filter::convert_where_clause,
        get_tx_conn,
        helpers::{check_hook_depth, hook_user, resolve_collection},
    },
    service::{CountDocumentsInput, LuaReadHooks, ServiceContext, count_documents},
    typegen::lua::{LuaAnnotation, LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table},
};

/// Query passed to `crap.collections.count(collection, query)`.
#[derive(Debug, Default, Deserialize, LuaAnnotation)]
#[serde(default, deny_unknown_fields)]
#[lua(class = "crap.CountQuery")]
pub(crate) struct CountQueryInput {
    #[serde(rename = "where")]
    #[lua(
        rename = "where",
        ty = "table<string, crap.FilterValue | crap.OrCondition[]>",
        optional
    )]
    pub(crate) where_: Option<HashMap<String, serde_json::Value>>,
    /// Locale code for localized fields.
    #[lua(optional)]
    pub(crate) locale: Option<String>,
    /// Skip access control checks (default: `false`).
    #[lua(optional)]
    pub(crate) override_access: Option<bool>,
    /// Include draft documents (default: `false`).
    #[lua(optional)]
    pub(crate) draft: Option<bool>,
    /// Count only soft-deleted (trash) documents (default: `false`).
    #[lua(optional)]
    pub(crate) trash: Option<bool>,
    /// FTS5 full-text search query.
    #[lua(optional)]
    pub(crate) search: Option<String>,
}

impl FromLua for CountQueryInput {
    fn from_lua(value: Value, lua: &Lua) -> LuaResult<Self> {
        match value {
            Value::Nil => Ok(Self::default()),
            other => lua.from_value(other),
        }
    }
}

impl CountQueryInput {
    pub(crate) fn into_find_query(self) -> LuaResult<FindQuery> {
        let filters = match self.where_ {
            Some(w) => convert_where_clause(w)?,
            None => Vec::new(),
        };
        Ok(FindQuery::builder()
            .filters(filters)
            .search(self.search)
            .build())
    }
}

/// State threaded into `crap.collections.count`.
pub(crate) struct CollectionsCountState {
    pub(crate) registry: Arc<Registry>,
    pub(crate) locale_config: LocaleConfig,
}

/// Count documents matching a query.
/// Inside hooks, runs within the parent operation's transaction.
#[lua_fn(
    path = "crap.collections.count",
    returns_doc = "Number of matching documents.",
    auto_tx
)]
fn collections_count(
    state: &CollectionsCountState,
    lua: &Lua,
    #[lua(doc = "Collection slug.")] collection: String,
    #[lua(ty = "crap.CountQuery", doc = "Optional query parameters.")] query: Option<
        CountQueryInput,
    >,
) -> LuaResult<i64> {
    count_inner(
        lua,
        &state.registry,
        &state.locale_config,
        &collection,
        query.unwrap_or_default(),
    )
}

lua_table! {
    name: crap_collections_count,
    path: "crap.collections",
    state: CollectionsCountState,
    fns: [collections_count],
}

/// Register `crap.collections.count(collection, query?)`. Parent
/// `crap.collections` must already exist.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_count(
    lua: &Lua,
    _table: &Table,
    registry: Arc<Registry>,
    locale_config: &LocaleConfig,
) -> Result<()> {
    register_crap_collections_count(
        lua,
        CollectionsCountState {
            registry,
            locale_config: locale_config.clone(),
        },
    )?;
    Ok(())
}

/// Core logic for `crap.collections.count`.
fn count_inner(
    lua: &Lua,
    reg: &Registry,
    lc: &LocaleConfig,
    collection: &str,
    query: CountQueryInput,
) -> LuaResult<i64> {
    let conn = get_tx_conn(lua)?;

    let locale_ctx =
        LocaleContext::from_locale_string(query.locale.as_deref(), lc).map_err(lua_err)?;
    let override_access = query.override_access.unwrap_or(false);
    let draft = query.draft.unwrap_or(false);
    let trash = query.trash.unwrap_or(false);
    let user = hook_user(lua);
    let def = resolve_collection(reg, collection)?;

    let find_query = query.into_find_query()?;
    let mut filters = find_query.filters;
    let search = find_query.search;

    normalize_filter_fields(&mut filters, &def.fields);

    // Depth guard: a before_read hook that counts the same collection
    // recurses — cap it like the write paths do.
    let (hooks_enabled, _guard) = check_hook_depth(lua, true, collection, "count");

    let hooks = LuaReadHooks::builder(lua)
        .user(user.as_ref())
        .override_access(override_access)
        .hooks_enabled(hooks_enabled)
        .build();

    let ctx = ServiceContext::collection(collection, &def)
        .conn(conn)
        .read_hooks(&hooks)
        .user(user.as_ref())
        .override_access(override_access)
        .build();

    let input = CountDocumentsInput::builder(&filters)
        .locale_ctx(locale_ctx.as_ref())
        .search(search.as_deref())
        .include_drafts(draft)
        .trash(trash)
        .build();

    count_documents(&ctx, &input).map_err(lua_err)
}
