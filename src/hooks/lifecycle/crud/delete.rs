//! Registration of `crap.collections.delete` Lua function.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table};
use serde_json::Value;

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, Document, SharedRegistry, upload},
    db::{DbConnection, query},
    hooks::{
        HookContext, HookEvent,
        lifecycle::{LuaStorage, execution::run_hooks_inner},
    },
};

use super::{get_tx_conn, helpers::*};

/// Shared context for a delete operation.
struct DeleteCtx<'a> {
    collection: &'a str,
    id: &'a str,
    is_hard: bool,
    user: Option<&'a Document>,
    ui_locale: Option<&'a str>,
}

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
    let ui_locale = hook_ui_locale(lua);
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
        lua,
        override_access,
        access_ref,
        Some(&id),
        &mut vec![],
        "Delete access denied",
    )?;

    let ctx = DeleteCtx {
        collection: &collection,
        id: &id,
        is_hard,
        user: user.as_ref(),
        ui_locale: ui_locale.as_deref(),
    };

    let (hooks_enabled, _guard) = check_hook_depth(lua, run_hooks, &collection, "delete");

    check_ref_count(conn, &ctx)?;

    let upload_doc_fields = if def.is_upload_collection() {
        query::find_by_id(conn, &collection, &def, &id, None)
            .ok()
            .flatten()
            .map(|d| d.fields.clone())
    } else {
        None
    };

    if hooks_enabled {
        run_delete_hook(lua, &def, HookEvent::BeforeDelete, &ctx)?;
    }

    if is_hard {
        query::ref_count::before_hard_delete(conn, &collection, &id, &def.fields, lc)
            .map_err(|e| RuntimeError(format!("ref count error: {e:#}")))?;
    }

    execute_delete(conn, &ctx)?;
    cleanup_after_delete(lua, conn, &def, &ctx, upload_doc_fields)?;

    if hooks_enabled {
        run_delete_hook(lua, &def, HookEvent::AfterDelete, &ctx)?;
    }

    Ok(true)
}

/// Block deletion if the document is referenced by others.
fn check_ref_count(conn: &dyn DbConnection, ctx: &DeleteCtx<'_>) -> mlua::Result<()> {
    if !ctx.is_hard {
        return Ok(());
    }

    let count = query::ref_count::get_ref_count(conn, ctx.collection, ctx.id)
        .map_err(|e| RuntimeError(format!("ref count check error: {e:#}")))?
        .unwrap_or(0);

    if count > 0 {
        return Err(RuntimeError(format!(
            "Cannot delete '{}' from '{}': referenced by {count} document(s)",
            ctx.id, ctx.collection
        )));
    }
    Ok(())
}

/// Execute the actual delete or soft-delete.
fn execute_delete(conn: &dyn DbConnection, ctx: &DeleteCtx<'_>) -> mlua::Result<()> {
    if ctx.is_hard {
        let deleted = query::delete(conn, ctx.collection, ctx.id)
            .map_err(|e| RuntimeError(format!("delete error: {e:#}")))?;
        if !deleted {
            return Err(RuntimeError(format!(
                "Document '{}' not found in '{}'",
                ctx.id, ctx.collection
            )));
        }
    } else {
        let deleted = query::soft_delete(conn, ctx.collection, ctx.id)
            .map_err(|e| RuntimeError(format!("soft_delete error: {e:#}")))?;
        if !deleted {
            return Err(RuntimeError(format!(
                "Document '{}' not found or already deleted in '{}'",
                ctx.id, ctx.collection
            )));
        }
    }
    Ok(())
}

/// FTS sync, image queue cleanup, and upload file cleanup after delete.
fn cleanup_after_delete(
    lua: &Lua,
    conn: &dyn DbConnection,
    def: &CollectionDefinition,
    ctx: &DeleteCtx<'_>,
    upload_doc_fields: Option<std::collections::HashMap<String, Value>>,
) -> mlua::Result<()> {
    if conn.supports_fts() {
        query::fts::fts_delete(conn, ctx.collection, ctx.id)
            .map_err(|e| RuntimeError(format!("FTS delete error: {e:#}")))?;
    }

    if def.is_upload_collection() {
        let _ = query::images::delete_entries_for_document(conn, ctx.collection, ctx.id);
    }

    if ctx.is_hard
        && let Some(fields) = upload_doc_fields
        && let Some(lua_storage) = lua.app_data_ref::<LuaStorage>()
    {
        upload::delete_upload_files(&*lua_storage.0, &fields);
    }

    Ok(())
}

/// Run a before/after delete hook.
fn run_delete_hook(
    lua: &Lua,
    def: &CollectionDefinition,
    event: HookEvent,
    ctx: &DeleteCtx<'_>,
) -> mlua::Result<()> {
    let hook_ctx = HookContext::builder(ctx.collection, "delete")
        .data([("id".to_string(), Value::String(ctx.id.to_string()))].into())
        .user(ctx.user)
        .ui_locale(ctx.ui_locale)
        .build();
    run_hooks_inner(lua, &def.hooks, event, hook_ctx)
        .map_err(|e| RuntimeError(format!("delete hook error: {e:#}")))?;
    Ok(())
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
