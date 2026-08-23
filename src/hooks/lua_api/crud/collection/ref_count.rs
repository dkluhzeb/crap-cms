//! Registration of `crap.collections.ref_count` Lua function.

use std::sync::Arc;

use crate::hooks::lua_api::utils::lua_err;
use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table};

use crate::{
    core::Registry,
    hooks::lua_api::crud::{
        get_tx_conn,
        helpers::{hook_user, resolve_collection},
    },
    service::{FindByIdInput, LuaReadHooks, ServiceContext, document_info, find_document_by_id},
    typegen::lua::{LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table},
};

/// Return the incoming-reference count for a document. Used to gauge
/// whether deletion would be blocked by relationships from other
/// collections.
/// Inside hooks, runs within the parent operation's transaction.
#[lua_fn(
    path = "crap.collections.ref_count",
    returns_doc = "Number of incoming references.",
    auto_tx
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

    // Gate on read access: this is the only read-shaped op that previously
    // returned data for arbitrary ids with no access check. Reuse the read
    // path's visibility gate (read/draft/trash + row constraints) via a
    // depth-0 lookup — the count is only returned for a document the caller
    // may actually read.
    let user = hook_user(lua);
    let hooks = LuaReadHooks::builder(lua).user(user.as_ref()).build();

    let read_ctx = ServiceContext::collection(collection, &def)
        .conn(conn)
        .read_hooks(&hooks)
        .user(user.as_ref())
        .build();

    let visible = find_document_by_id(&read_ctx, &FindByIdInput::builder(id).depth(0).build())
        .map_err(lua_err)?;

    if visible.is_none() {
        return Err(RuntimeError(format!(
            "Document '{id}' not found in '{collection}' or not readable"
        )));
    }

    let ctx = ServiceContext::collection(collection, &def)
        .conn(conn)
        .build();
    document_info::get_ref_count(&ctx, id).map_err(lua_err)
}
