//! Registration of `crap.collections.restore_version` Lua function.

use std::sync::Arc;

use anyhow::Result;
use mlua::{Error::RuntimeError, FromLua, Lua, LuaSerdeExt, Result as LuaResult, Table, Value};
use serde::Deserialize;

use crate::{
    config::LocaleConfig,
    core::Registry,
    hooks::{
        lifecycle::converters::document_to_lua_table,
        lua_api::crud::{
            get_tx_conn,
            helpers::{hook_invalidation_transport, hook_lua_infra, hook_user, resolve_collection},
        },
    },
    service::{LuaWriteHooks, ServiceContext, restore_collection_version},
    typegen::lua::{LuaAnnotation, LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table},
};

/// Optional options for `crap.collections.restore_version`.
#[derive(Default, Deserialize, LuaAnnotation)]
#[serde(default, deny_unknown_fields)]
#[lua(class = "crap.RestoreVersionOptions")]
pub(crate) struct RestoreVersionOptions {
    /// Skip access control checks (default: `false`). Set to `true` in
    /// trusted internal code (jobs, migrations) to bypass collection-level
    /// access checks.
    #[lua(optional)]
    pub(crate) override_access: bool,
}

impl FromLua for RestoreVersionOptions {
    fn from_lua(value: Value, lua: &Lua) -> LuaResult<Self> {
        match value {
            Value::Nil => Ok(Self::default()),
            other => lua.from_value(other),
        }
    }
}

/// State threaded into `crap.collections.restore_version`.
pub(crate) struct CollectionsRestoreVersionState {
    pub(crate) registry: Arc<Registry>,
    pub(crate) locale_config: LocaleConfig,
}

/// Restore a previous version: copies the snapshot data back onto the
/// parent document and writes a new version row. Returns the restored
/// document. Only available on collections with `versions` enabled.
/// Inside hooks, runs within the parent operation's transaction.
#[lua_fn(
    path = "crap.collections.restore_version",
    returns = "crap.Document",
    auto_tx
)]
fn collections_restore_version(
    state: &CollectionsRestoreVersionState,
    lua: &Lua,
    #[lua(doc = "Collection slug.")] collection: String,
    #[lua(doc = "Parent document ID.")] id: String,
    #[lua(doc = "Version row ID to restore.")] version_id: String,
    #[lua(ty = "crap.RestoreVersionOptions", doc = "Optional options.")] opts: Option<
        RestoreVersionOptions,
    >,
) -> LuaResult<Value> {
    let opts = opts.unwrap_or_default();
    let reg = &state.registry;
    let lc = &state.locale_config;

    let conn = get_tx_conn(lua)?;

    let user = hook_user(lua);
    let lua_infra = hook_lua_infra(lua);
    let def = resolve_collection(reg, &collection)?;

    let write_hooks = LuaWriteHooks::builder(lua)
        .user(user.as_ref())
        .override_access(opts.override_access)
        .build();

    let ctx = ServiceContext::collection(&collection, &def)
        .conn(conn)
        .write_hooks(&write_hooks)
        .user(user.as_ref())
        .override_access(opts.override_access)
        .lua_infra(lua_infra.as_ref())
        .invalidation_transport(hook_invalidation_transport(lua))
        .build();

    let doc = restore_collection_version(&ctx, &id, &version_id, lc)
        .map_err(|e| RuntimeError(format!("{e}")))?;

    Ok(Value::Table(document_to_lua_table(lua, &doc)?))
}

lua_table! {
    name: crap_collections_restore_version,
    path: "crap.collections",
    state: CollectionsRestoreVersionState,
    fns: [collections_restore_version],
}

/// Register `crap.collections.restore_version(collection, id, version_id, opts?)`.
/// Parent `crap.collections` must already exist.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_restore_version(
    lua: &Lua,
    _table: &Table,
    registry: Arc<Registry>,
    locale_config: &LocaleConfig,
) -> Result<()> {
    register_crap_collections_restore_version(
        lua,
        CollectionsRestoreVersionState {
            registry,
            locale_config: locale_config.clone(),
        },
    )?;
    Ok(())
}
