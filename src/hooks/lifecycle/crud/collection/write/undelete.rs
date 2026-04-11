//! Registration of `crap.collections.undelete` Lua function.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table};

use crate::{
    core::SharedRegistry,
    hooks::lifecycle::crud::{get_tx_conn, helpers::*},
    service::{self, LuaWriteHooks, ServiceContext},
};

/// Undelete a soft-deleted document by ID.
///
/// Validates that the collection supports soft delete, then delegates to
/// `service::undelete_document_core` which handles access checks internally.
fn undelete_document(
    lua: &Lua,
    reg: &SharedRegistry,
    collection: &str,
    id: &str,
    opts: &Option<Table>,
) -> mlua::Result<bool> {
    // SAFETY: pointer valid for hook call duration — see TxContext pattern
    let conn_ptr = get_tx_conn(lua)?;
    let conn = unsafe { &*conn_ptr };

    let override_access = get_opt_bool(opts, "overrideAccess", false)?;
    let user = hook_user(lua);
    let def = resolve_collection(reg, collection)?;

    if !def.soft_delete {
        return Err(RuntimeError(format!(
            "Collection '{}' does not have soft_delete enabled",
            collection
        )));
    }

    let wh = LuaWriteHooks::builder(lua)
        .user(user.as_ref())
        .override_access(override_access)
        .hooks_enabled(false)
        .run_validation(false)
        .build();

    let ctx = ServiceContext::collection(collection, &def)
        .conn(conn)
        .write_hooks(&wh)
        .user(user.as_ref())
        .override_access(override_access)
        .build();

    service::undelete_document_core(&ctx, id).map_err(|e| RuntimeError(format!("{e}")))?;

    Ok(true)
}

/// Register `crap.collections.undelete(collection, id, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_undelete(lua: &Lua, table: &Table, registry: SharedRegistry) -> Result<()> {
    let undelete_fn = lua.create_function(
        move |lua, (collection, id, opts): (String, String, Option<Table>)| {
            undelete_document(lua, &registry, &collection, &id, &opts)
        },
    )?;
    table.set("undelete", undelete_fn)?;

    Ok(())
}
