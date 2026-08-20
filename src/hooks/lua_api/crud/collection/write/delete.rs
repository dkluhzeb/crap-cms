//! Registration of `crap.collections.delete` Lua function.

use std::sync::Arc;

use anyhow::Result;
use mlua::{Error::RuntimeError, FromLua, Lua, LuaSerdeExt, Result as LuaResult, Table, Value};
use serde::Deserialize;

use crate::{
    config::LocaleConfig,
    core::Registry,
    hooks::{
        lifecycle::LuaStorage,
        lua_api::crud::{
            get_tx_conn,
            helpers::{
                check_hook_depth, hook_invalidation_transport, hook_lua_infra, hook_user,
                resolve_collection,
            },
        },
    },
    service::{LuaWriteHooks, ServiceContext, delete_document},
    typegen::lua::{LuaAnnotation, LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table},
};

/// Optional options for `crap.collections.delete`.
#[derive(Deserialize, LuaAnnotation)]
#[serde(default, deny_unknown_fields)]
#[lua(class = "crap.DeleteOptions")]
pub(crate) struct DeleteOptions {
    /// Skip access control checks (default: `false`). Set to `true` in
    /// trusted internal code to bypass collection-level access for the
    /// current user.
    #[lua(optional)]
    pub(crate) override_access: bool,
    /// Run lifecycle hooks (default: `true`). Set `false` to bypass hooks.
    #[lua(optional)]
    pub(crate) hooks: bool,
    /// Bypass `soft_delete` and remove the row permanently. Mirrors the
    /// same flag on the gRPC/HTTP delete handlers.
    #[lua(optional)]
    pub(crate) force_hard_delete: bool,
    /// Emit a live-update event for the deleted document (default: `true`).
    /// Set `false` for a quiet delete.
    #[lua(optional)]
    pub(crate) events: bool,
}

impl Default for DeleteOptions {
    fn default() -> Self {
        Self {
            override_access: false,
            hooks: true,
            force_hard_delete: false,
            events: true,
        }
    }
}

impl FromLua for DeleteOptions {
    fn from_lua(value: Value, lua: &Lua) -> LuaResult<Self> {
        match value {
            Value::Nil => Ok(Self::default()),
            other => lua.from_value(other),
        }
    }
}

/// State threaded into `crap.collections.delete`.
pub(crate) struct CollectionsDeleteState {
    pub(crate) registry: Arc<Registry>,
    pub(crate) locale_config: LocaleConfig,
}

/// Delete a document.
/// Inside hooks, runs within the parent operation's transaction.
#[lua_fn(
    path = "crap.collections.delete",
    returns_doc = "True on successful delete.",
    auto_tx
)]
fn collections_delete(
    state: &CollectionsDeleteState,
    lua: &Lua,
    #[lua(doc = "Collection slug.")] collection: String,
    #[lua(doc = "Document ID.")] id: String,
    #[lua(ty = "crap.DeleteOptions", doc = "Optional options.")] opts: Option<DeleteOptions>,
) -> LuaResult<bool> {
    let opts = opts.unwrap_or_default();
    let reg = &state.registry;
    let lc = &state.locale_config;

    let conn = get_tx_conn(lua)?;

    let user = hook_user(lua);
    let lua_infra = hook_lua_infra(lua);
    let mut def = resolve_collection(reg, &collection)?;

    if opts.force_hard_delete && def.soft_delete {
        def.make_hard_delete();
    }

    let (hooks_enabled, _guard) = check_hook_depth(lua, opts.hooks, &collection, "delete");

    let write_hooks = LuaWriteHooks::builder(lua)
        .user(user.as_ref())
        .override_access(opts.override_access)
        .registry(Some(reg.as_ref()))
        .hooks_enabled(hooks_enabled)
        .build();

    let storage = lua.app_data_ref::<LuaStorage>().map(|s| s.0.clone());
    let invalidation_transport = hook_invalidation_transport(lua);

    let ctx = ServiceContext::collection(&collection, &def)
        .conn(conn)
        .write_hooks(&write_hooks)
        .user(user.as_ref())
        .override_access(opts.override_access)
        .invalidation_transport(invalidation_transport)
        .emit_events(opts.events)
        .lua_infra(lua_infra.as_ref())
        .build();

    delete_document(&ctx, &id, storage.as_deref(), Some(lc))
        .map_err(|e| RuntimeError(format!("delete error: {e:#}")))?;

    Ok(true)
}

lua_table! {
    name: crap_collections_delete,
    path: "crap.collections",
    state: CollectionsDeleteState,
    fns: [collections_delete],
}

/// Register `crap.collections.delete(collection, id, opts?)`. Parent
/// `crap.collections` must already exist.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_delete(
    lua: &Lua,
    _table: &Table,
    registry: Arc<Registry>,
    locale_config: &LocaleConfig,
) -> Result<()> {
    register_crap_collections_delete(
        lua,
        CollectionsDeleteState {
            registry,
            locale_config: locale_config.clone(),
        },
    )?;
    Ok(())
}
