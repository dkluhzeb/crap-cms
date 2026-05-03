//! Registration of `crap.collections.delete_many` Lua function.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table};

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, SharedRegistry, upload},
    db::{FilterClause, LocaleContext, query::filter::normalize_filter_fields},
    hooks::lifecycle::{
        LuaStorage,
        converters::lua_table_to_find_query,
        crud::{get_tx_conn, helpers::*},
    },
    service::{self, DeleteManyOptions, LuaWriteHooks, ServiceContext, validate_user_filters},
};

/// Resolve the access function for delete operations.
fn resolve_delete_access(def: &CollectionDefinition, soft_delete: bool) -> Option<&str> {
    if soft_delete {
        def.access.resolve_trash()
    } else {
        def.access.delete.as_deref()
    }
}

/// Build filters for the bulk delete query, enforcing access constraints.
fn build_delete_filters(
    lua: &Lua,
    def: &CollectionDefinition,
    collection: &str,
    soft_delete: bool,
    override_access: bool,
    lc: &LocaleConfig,
    query_table: &Table,
) -> mlua::Result<(Vec<FilterClause>, Option<LocaleContext>)> {
    let locale_ctx = LocaleContext::from_locale_string(
        get_opt_string(&Some(query_table.clone()), "locale")?.as_deref(),
        lc,
    )
    .map_err(|e| RuntimeError(e.to_string()))?;

    let (mut find_query, _) = lua_table_to_find_query(query_table)?;
    normalize_filter_fields(&mut find_query.filters, &def.fields);
    validate_user_filters(&find_query.filters).map_err(|e| RuntimeError(format!("{e}")))?;

    let access_ref = resolve_delete_access(def, soft_delete);

    enforce_access(
        lua,
        &EnforceAccessParams {
            slug: collection,
            override_access,
            access_fn: access_ref,
            id: None,
            deny_msg: "Delete access denied",
            injecting_status: false,
        },
        &mut find_query.filters,
    )?;

    Ok((find_query.filters, locale_ctx))
}

/// Delete multiple documents matching a query.
///
/// Delegates to `service::delete_many` which handles the per-document lifecycle
/// (ref count checks, before/after delete hooks, the delete itself, FTS/image cleanup).
/// Referenced documents are skipped (not errored).
fn delete_many_documents(
    lua: &Lua,
    reg: &SharedRegistry,
    lc: &LocaleConfig,
    collection: &str,
    query_table: &Table,
    opts: &Option<Table>,
) -> mlua::Result<Table> {
    // SAFETY: pointer valid for hook call duration — see TxContext pattern
    let conn_ptr = get_tx_conn(lua)?;
    let conn = unsafe { &*conn_ptr };

    let override_access = get_opt_bool(opts, "overrideAccess", false)?;
    let run_hooks = get_opt_bool(opts, "hooks", true)?;
    let force_hard_delete = get_opt_bool(opts, "forceHardDelete", false)?;

    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let lua_infra = hook_lua_infra(lua);
    let def = resolve_collection(reg, collection)?;
    let soft_delete = def.soft_delete && !force_hard_delete;

    let (filters, _locale_ctx) = build_delete_filters(
        lua,
        &def,
        collection,
        soft_delete,
        override_access,
        lc,
        query_table,
    )?;

    let (hooks_enabled, _guard) = check_hook_depth(lua, run_hooks, collection, "delete_many");

    let r = reg
        .read()
        .map_err(|e| RuntimeError(format!("Registry lock: {e:#}")))?;

    let write_hooks = LuaWriteHooks::builder(lua)
        .user(user.as_ref())
        .ui_locale(ui_locale.as_deref())
        .override_access(override_access)
        .registry(Some(&r))
        .hooks_enabled(hooks_enabled)
        .build();

    let mut service_def = def.clone();
    if force_hard_delete {
        service_def.soft_delete = false;
    }

    let invalidation_transport = hook_invalidation_transport(lua);

    let ctx = ServiceContext::collection(collection, &service_def)
        .conn(conn)
        .write_hooks(&write_hooks)
        .user(user.as_ref())
        .override_access(override_access)
        .invalidation_transport(invalidation_transport)
        .lua_infra(lua_infra.as_ref())
        .build();

    let delete_opts = DeleteManyOptions {
        run_hooks: hooks_enabled,
        ..Default::default()
    };

    let svc_result = service::delete_many(&ctx, filters, lc, &delete_opts)
        .map_err(|e| RuntimeError(format!("{e}")))?;

    // Clean up upload files for hard deletes.
    if !service_def.soft_delete
        && let Some(lua_storage) = lua.app_data_ref::<LuaStorage>()
    {
        for fields in &svc_result.upload_fields_to_clean {
            upload::delete_upload_files(&*lua_storage.0, fields);
        }
    }

    let result = lua.create_table()?;
    result.set("deleted", svc_result.hard_deleted + svc_result.soft_deleted)?;
    result.set("skipped", svc_result.skipped)?;

    Ok(result)
}

/// Register `crap.collections.delete_many(collection, query, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_delete_many(
    lua: &Lua,
    table: &Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let lc = locale_config.clone();
    let delete_many_fn = lua.create_function(
        move |lua, (collection, query_table, opts): (String, Table, Option<Table>)| {
            delete_many_documents(lua, &registry, &lc, &collection, &query_table, &opts)
        },
    )?;

    table.set("delete_many", delete_many_fn)?;

    Ok(())
}
