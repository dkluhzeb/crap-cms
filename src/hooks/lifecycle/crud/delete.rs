//! Registration of `crap.collections.delete` Lua function.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table};

use crate::{
    config::LocaleConfig,
    core::{SharedRegistry, upload},
    hooks::lifecycle::LuaStorage,
    service::{LuaWriteHooks, delete_document_core},
};

use super::{get_tx_conn, helpers::*};

/// Execute the delete operation.
fn delete_document(
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
    let override_access = get_opt_bool(&opts, "overrideAccess", false)?;
    let run_hooks = get_opt_bool(&opts, "hooks", true)?;
    let force_hard_delete = get_opt_bool(&opts, "forceHardDelete", false)?;
    let def = resolve_collection(reg, &collection)?;

    let is_hard = !def.soft_delete || force_hard_delete;
    let access_ref = if !is_hard {
        def.access.resolve_trash()
    } else {
        def.access.delete.as_deref()
    };
    enforce_access(
        lua, override_access, access_ref,
        Some(&id), &mut vec![], "Delete access denied",
    )?;

    let (hooks_enabled, _guard) = check_hook_depth(lua, run_hooks, &collection, "delete");

    let r = reg.read().map_err(|e| RuntimeError(format!("Registry lock: {e:#}")))?;
    let write_hooks = LuaWriteHooks {
        lua,
        user: user.as_ref(),
        ui_locale: None,
        override_access,
        registry: Some(&r),
        hooks_enabled,
        run_validation: run_hooks,
    };

    let result = delete_document_core(conn, &write_hooks, &collection, &id, &def, user.as_ref(), Some(lc))
        .map_err(|e| RuntimeError(format!("delete error: {e:#}")))?;

    // Clean up upload files after successful delete (skip for soft-delete)
    if is_hard
        && let Some(fields) = result.upload_doc_fields
        && let Some(lua_storage) = lua.app_data_ref::<LuaStorage>()
    {
        upload::delete_upload_files(&*lua_storage.0, &fields);
    }

    Ok(true)
}

/// Register `crap.collections.delete(collection, id, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(super) fn register_delete(
    lua: &Lua,
    table: &Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let lc = locale_config.clone();
    let delete_fn = lua.create_function(
        move |lua, (collection, id, opts): (String, String, Option<Table>)| {
            delete_document(lua, &registry, &lc, collection, id, opts)
        },
    )?;
    table.set("delete", delete_fn)?;
    Ok(())
}
