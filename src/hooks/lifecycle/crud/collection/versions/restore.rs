//! Registration of `crap.collections.restore_version` Lua function.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table, Value};

use crate::{
    config::LocaleConfig,
    core::SharedRegistry,
    hooks::lifecycle::{
        converters::document_to_lua_table,
        crud::{get_tx_conn, helpers::*},
    },
    service::{LuaWriteHooks, restore_collection_version_core},
};

/// Core logic for `crap.collections.restore_version`.
fn restore_version_inner(
    lua: &Lua,
    reg: &SharedRegistry,
    lc: &LocaleConfig,
    collection: String,
    id: String,
    version_id: String,
) -> LuaResult<Value> {
    // SAFETY: pointer valid for hook call duration — see TxContext pattern
    let conn_ptr = get_tx_conn(lua)?;
    let conn = unsafe { &*conn_ptr };

    let user = hook_user(lua);
    let def = resolve_collection(reg, &collection)?;

    let write_hooks = LuaWriteHooks::builder(lua)
        .user(user.as_ref())
        .override_access(true)
        .build();

    let doc = restore_collection_version_core(
        conn,
        &write_hooks,
        &collection,
        &def,
        &id,
        &version_id,
        lc,
        user.as_ref(),
    )
    .map_err(|e| RuntimeError(format!("{e}")))?;

    Ok(Value::Table(document_to_lua_table(lua, &doc)?))
}

/// Register `crap.collections.restore_version(collection, id, version_id)`.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_restore_version(
    lua: &Lua,
    table: &Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let lc = locale_config.clone();
    let restore_version_fn = lua.create_function(
        move |lua, (collection, id, version_id): (String, String, String)| {
            restore_version_inner(lua, &registry, &lc, collection, id, version_id)
        },
    )?;

    table.set("restore_version", restore_version_fn)?;

    Ok(())
}
