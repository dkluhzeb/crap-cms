//! Registration of `crap.collections.ref_count` Lua function.

use std::sync::Arc;

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table};

use crate::{
    core::Registry,
    hooks::lua_api::crud::{get_tx_conn, helpers::resolve_collection},
    service::{ServiceContext, document_info},
    typegen::lua::{LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table},
};

/// Return the incoming-reference count for a document. Used to gauge
/// whether deletion would be blocked by relationships from other
/// collections.
/// Inside hooks, runs within the parent operation's transaction.
#[lua_fn(
    path = "crap.collections.ref_count",
    returns_doc = "Number of incoming references."
)]
fn collections_ref_count(
    state: &Arc<Registry>,
    lua: &Lua,
    #[lua(doc = "Collection slug.")] collection: String,
    #[lua(doc = "Document ID.")] id: String,
) -> LuaResult<i64> {
    ref_count_inner(lua, state, &collection, &id)
}

lua_table! {
    name: crap_collections_ref_count,
    path: "crap.collections",
    state: Arc<Registry>,
    fns: [collections_ref_count],
}

/// Register `crap.collections.ref_count(collection, id)`. Parent
/// `crap.collections` must already exist.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_ref_count(lua: &Lua, _table: &Table, registry: Arc<Registry>) -> Result<()> {
    register_crap_collections_ref_count(lua, registry)?;
    Ok(())
}

/// Core logic for `crap.collections.ref_count`.
fn ref_count_inner(lua: &Lua, reg: &Registry, collection: &str, id: &str) -> LuaResult<i64> {
    let conn = get_tx_conn(lua)?;

    let def = resolve_collection(reg, collection)?;

    let ctx = ServiceContext::collection(collection, &def)
        .conn(conn)
        .build();
    document_info::get_ref_count(&ctx, id).map_err(|e| RuntimeError(format!("{e}")))
}
