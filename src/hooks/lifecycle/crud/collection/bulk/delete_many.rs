//! Registration of `crap.collections.delete_many` Lua function.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table};

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, Document, SharedRegistry, upload},
    db::{
        DbConnection, FindQuery, LocaleContext,
        query::{self, filter::normalize_filter_fields},
    },
    hooks::lifecycle::{
        LuaStorage,
        converters::lua_table_to_find_query,
        crud::{get_tx_conn, helpers::*},
    },
    service::{LuaWriteHooks, ServiceError, delete_document_core},
};

/// Context for bulk delete operations.
struct DeleteManyCtx<'a> {
    collection: &'a str,
    soft_delete: bool,
    override_access: bool,
    draft: bool,
}

/// Resolve the access function for delete operations.
fn resolve_delete_access(def: &CollectionDefinition, soft_delete: bool) -> Option<&str> {
    if soft_delete {
        def.access.resolve_trash()
    } else {
        def.access.delete.as_deref()
    }
}

/// Find documents matching the query, enforcing access control and draft filtering.
fn find_docs_for_deletion(
    lua: &Lua,
    conn: &dyn DbConnection,
    def: &CollectionDefinition,
    ctx: &DeleteManyCtx<'_>,
    lc: &LocaleConfig,
    query_table: &Table,
) -> mlua::Result<Vec<Document>> {
    let locale_ctx = LocaleContext::from_locale_string(
        get_opt_string(&Some(query_table.clone()), "locale")?.as_deref(),
        lc,
    );

    let (mut find_query, _) = lua_table_to_find_query(query_table)?;
    normalize_filter_fields(&mut find_query.filters, &def.fields);
    add_draft_filter(def, ctx.draft, &mut find_query.filters);

    let access_ref = resolve_delete_access(def, ctx.soft_delete);
    enforce_access(
        lua,
        ctx.override_access,
        access_ref,
        None,
        &mut find_query.filters,
        "Delete access denied",
    )?;

    let mut find_all = FindQuery::new();
    find_all.filters = find_query.filters;
    // Internal batch lookup for bulk mutation — not a user-facing read.
    let docs = query::find(conn, ctx.collection, def, &find_all, locale_ctx.as_ref())
        .map_err(|e| RuntimeError(format!("find error: {e:#}")))?;

    // Per-doc access check is handled inside delete_document_core
    // via WriteHooks::check_access.

    Ok(docs)
}

/// Delete multiple documents matching a query.
///
/// For each matched document: delegates to `service::delete_document_core` which handles
/// ref count checks, before/after delete hooks, the delete itself, FTS/image cleanup.
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
    let draft = get_opt_bool(opts, "draft", false)?;

    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let def = resolve_collection(reg, collection)?;
    let soft_delete = def.soft_delete && !force_hard_delete;

    let ctx = DeleteManyCtx {
        collection,
        soft_delete,
        override_access,
        draft,
    };

    let docs = find_docs_for_deletion(lua, conn, &def, &ctx, lc, query_table)?;
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

    let mut deleted = 0i64;
    let mut skipped = 0i64;

    for doc in &docs {
        match delete_document_core(
            conn,
            &write_hooks,
            collection,
            &doc.id,
            &service_def,
            user.as_ref(),
            Some(lc),
        ) {
            Ok(result) => {
                // Clean up upload files for hard deletes
                if !service_def.soft_delete
                    && let Some(ref fields) = result.upload_doc_fields
                    && let Some(lua_storage) = lua.app_data_ref::<LuaStorage>()
                {
                    upload::delete_upload_files(&*lua_storage.0, fields);
                }
                deleted += 1;
            }
            Err(ServiceError::Referenced { .. }) => {
                skipped += 1;
            }
            Err(e) => return Err(RuntimeError(format!("{e}"))),
        }
    }

    let result = lua.create_table()?;
    result.set("deleted", deleted)?;
    result.set("skipped", skipped)?;

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
