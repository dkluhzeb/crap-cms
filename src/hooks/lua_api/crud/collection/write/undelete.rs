//! Registration of `crap.collections.undelete` Lua function.

use std::sync::Arc;

use anyhow::Result;
use mlua::{Error::RuntimeError, FromLua, Lua, LuaSerdeExt, Result as LuaResult, Table, Value};
use serde::Deserialize;

use crate::{
    core::Registry,
    hooks::lua_api::crud::{
        get_tx_conn,
        helpers::{hook_lua_infra, hook_user, resolve_collection},
    },
    service::{LuaWriteHooks, ServiceContext, undelete_document},
    typegen::lua::{LuaAnnotation, LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table},
};

/// Optional options for `crap.collections.undelete`.
#[derive(Default, Deserialize, LuaAnnotation)]
#[serde(default)]
#[lua(class = "crap.UndeleteOptions")]
pub(crate) struct UndeleteOptions {
    /// Skip access control checks (default: `false`).
    #[serde(rename = "overrideAccess")]
    #[lua(rename = "overrideAccess", optional)]
    pub(crate) override_access: bool,
}

impl FromLua for UndeleteOptions {
    fn from_lua(value: Value, lua: &Lua) -> LuaResult<Self> {
        match value {
            Value::Nil => Ok(Self::default()),
            other => lua.from_value(other),
        }
    }
}

/// Restore a soft-deleted document. Only available on collections with
/// `soft_delete` enabled.
/// Inside hooks, runs within the parent operation's transaction.
#[lua_fn(
    path = "crap.collections.undelete",
    returns_doc = "True when the document was successfully restored."
)]
fn collections_undelete(
    state: &Arc<Registry>,
    lua: &Lua,
    #[lua(doc = "Collection slug.")] collection: String,
    #[lua(doc = "Document ID.")] id: String,
    #[lua(ty = "crap.UndeleteOptions", doc = "Optional options.")] opts: Option<UndeleteOptions>,
) -> LuaResult<bool> {
    let opts = opts.unwrap_or_default();
    let conn = get_tx_conn(lua)?;

    let user = hook_user(lua);
    let lua_infra = hook_lua_infra(lua);
    let def = resolve_collection(state, &collection)?;

    if !def.soft_delete {
        return Err(RuntimeError(format!(
            "Collection '{collection}' does not have soft_delete enabled"
        )));
    }

    let wh = LuaWriteHooks::builder(lua)
        .user(user.as_ref())
        .override_access(opts.override_access)
        .hooks_enabled(false)
        .run_validation(false)
        .build();

    let ctx = ServiceContext::collection(&collection, &def)
        .conn(conn)
        .write_hooks(&wh)
        .user(user.as_ref())
        .override_access(opts.override_access)
        .lua_infra(lua_infra.as_ref())
        .build();

    undelete_document(&ctx, &id).map_err(|e| RuntimeError(format!("{e}")))?;

    Ok(true)
}

lua_table! {
    name: crap_collections_undelete,
    path: "crap.collections",
    state: Arc<Registry>,
    fns: [collections_undelete],
}

/// Register `crap.collections.undelete(collection, id, opts?)`. Parent
/// `crap.collections` must already exist.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_undelete(lua: &Lua, _table: &Table, registry: Arc<Registry>) -> Result<()> {
    register_crap_collections_undelete(lua, registry)?;
    Ok(())
}
