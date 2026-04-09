//! Registration of `crap.collections.ref_count` Lua function.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table};

use crate::{
    core::SharedRegistry,
    hooks::lifecycle::crud::{get_tx_conn, helpers::*},
    service::document_info,
};

/// Core logic for `crap.collections.ref_count`.
fn ref_count_inner(
    lua: &Lua,
    reg: &SharedRegistry,
    collection: String,
    id: String,
) -> LuaResult<i64> {
    // SAFETY: pointer valid for hook call duration — see TxContext pattern
    let conn_ptr = get_tx_conn(lua)?;
    let conn = unsafe { &*conn_ptr };

    // Validate collection exists
    let _def = resolve_collection(reg, &collection)?;

    document_info::get_ref_count(conn, &collection, &id).map_err(|e| RuntimeError(format!("{e}")))
}

/// Register `crap.collections.ref_count(collection, id)`.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_ref_count(lua: &Lua, table: &Table, registry: SharedRegistry) -> Result<()> {
    let ref_count_fn = lua.create_function(move |lua, (collection, id): (String, String)| {
        ref_count_inner(lua, &registry, collection, id)
    })?;

    table.set("ref_count", ref_count_fn)?;

    Ok(())
}
