//! Registration of `crap.collections.delete` Lua function.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table};

use crate::{
    config::LocaleConfig,
    core::SharedRegistry,
    hooks::lifecycle::{
        LuaStorage,
        crud::{get_tx_conn, helpers::*},
    },
    service::{LuaWriteHooks, ServiceContext, delete_document},
};

/// Execute the delete operation.
fn delete_document_lua(
    lua: &Lua,
    reg: &SharedRegistry,
    lc: &LocaleConfig,
    collection: String,
    id: String,
    opts: Option<Table>,
) -> mlua::Result<bool> {
    // SAFETY: pointer valid for hook call duration — see TxContext pattern
    let conn_ptr = get_tx_conn(lua)?;
    let conn = unsafe { &*conn_ptr };

    let user = hook_user(lua);
    let lua_infra = hook_lua_infra(lua);
    let override_access = get_opt_bool(&opts, "overrideAccess", false)?;
    let run_hooks = get_opt_bool(&opts, "hooks", true)?;
    let force_hard_delete = get_opt_bool(&opts, "forceHardDelete", false)?;
    let mut def = resolve_collection(reg, &collection)?;

    // `force_hard_delete` on a soft-delete collection must flip the def so
    // `delete_document_core` treats it as a hard delete. Mirrors the pattern
    // in gRPC handlers and Lua bulk `delete_many`. Without this, the option
    // was silently ignored and rows were soft-deleted regardless.
    if force_hard_delete && def.soft_delete {
        def.soft_delete = false;
    }

    // Collection-level access check is handled inside service::delete_document
    // via WriteHooks::check_access (respects override_access on LuaWriteHooks).

    let (hooks_enabled, _guard) = check_hook_depth(lua, run_hooks, &collection, "delete");

    let r = reg
        .read()
        .map_err(|e| RuntimeError(format!("Registry lock: {e:#}")))?;
    let write_hooks = LuaWriteHooks::builder(lua)
        .user(user.as_ref())
        .override_access(override_access)
        .registry(Some(&r))
        .hooks_enabled(hooks_enabled)
        .build();

    let storage = lua.app_data_ref::<LuaStorage>().map(|s| s.0.clone());
    let invalidation_transport = hook_invalidation_transport(lua);

    let ctx = ServiceContext::collection(&collection, &def)
        .conn(conn)
        .write_hooks(&write_hooks)
        .user(user.as_ref())
        .override_access(override_access)
        .invalidation_transport(invalidation_transport)
        .lua_infra(lua_infra.as_ref())
        .build();

    delete_document(&ctx, &id, storage.as_deref(), Some(lc))
        .map_err(|e| RuntimeError(format!("delete error: {e:#}")))?;

    Ok(true)
}

/// Register `crap.collections.delete(collection, id, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_delete(
    lua: &Lua,
    table: &Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let lc = locale_config.clone();
    let delete_fn = lua.create_function(
        move |lua, (collection, id, opts): (String, String, Option<Table>)| {
            delete_document_lua(lua, &registry, &lc, collection, id, opts)
        },
    )?;
    table.set("delete", delete_fn)?;
    Ok(())
}
