//! Registration of `crap.collections.delete_many` Lua function.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table};
use serde_json::Value;
use tracing::debug;

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, Document, SharedRegistry, upload},
    db::{
        DbConnection, FindQuery, LocaleContext,
        query::{self, filter::normalize_filter_fields},
    },
    hooks::{
        HookContext, HookEvent,
        lifecycle::{LuaStorage, converters::lua_table_to_find_query, execution::run_hooks_inner},
    },
};

use super::{get_tx_conn, helpers::*};

/// Context for bulk delete operations.
struct DeleteManyCtx<'a> {
    collection: &'a str,
    soft_delete: bool,
    override_access: bool,
    draft: bool,
    user: Option<&'a Document>,
    ui_locale: Option<&'a str>,
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
    let docs = query::find(conn, ctx.collection, def, &find_all, locale_ctx.as_ref())
        .map_err(|e| RuntimeError(format!("find error: {e:#}")))?;

    // Check per-doc delete access (all-or-nothing)
    for doc in &docs {
        enforce_access(
            lua,
            ctx.override_access,
            access_ref,
            Some(&doc.id),
            &mut vec![],
            &format!("Delete access denied for document {}", doc.id),
        )?;
    }

    Ok(docs)
}

/// Check if a document has incoming references and should be skipped.
fn should_skip_for_refs(
    conn: &dyn DbConnection,
    collection: &str,
    doc_id: &str,
) -> mlua::Result<bool> {
    let ref_count = query::ref_count::get_ref_count(conn, collection, doc_id)
        .map_err(|e| RuntimeError(format!("ref count check error: {e:#}")))?
        .unwrap_or(0);

    if ref_count > 0 {
        debug!(
            "Skipping delete of {}/{}: referenced by {} document(s)",
            collection, doc_id, ref_count
        );

        return Ok(true);
    }

    Ok(false)
}

/// Execute a single soft or hard delete, returning false if already deleted.
fn execute_single_delete(
    conn: &dyn DbConnection,
    ctx: &DeleteManyCtx<'_>,
    doc_id: &str,
) -> mlua::Result<bool> {
    if ctx.soft_delete {
        query::soft_delete(conn, ctx.collection, doc_id)
            .map_err(|e| RuntimeError(format!("soft_delete error: {e:#}")))
    } else {
        query::delete(conn, ctx.collection, doc_id)
            .map_err(|e| RuntimeError(format!("delete error: {e:#}")))
    }
}

/// FTS sync, image queue cleanup, and upload file cleanup for a deleted document.
fn cleanup_after_single_delete(
    lua: &Lua,
    conn: &dyn DbConnection,
    def: &CollectionDefinition,
    ctx: &DeleteManyCtx<'_>,
    doc: &Document,
) -> mlua::Result<()> {
    if conn.supports_fts() {
        query::fts::fts_delete(conn, ctx.collection, &doc.id)
            .map_err(|e| RuntimeError(format!("FTS delete error: {e:#}")))?;
    }

    if def.is_upload_collection() {
        let _ = query::images::delete_entries_for_document(conn, ctx.collection, &doc.id);
    }

    if !ctx.soft_delete
        && let Some(lua_storage) = lua.app_data_ref::<LuaStorage>()
    {
        upload::delete_upload_files(&*lua_storage.0, &doc.fields);
    }

    Ok(())
}

/// Run a before/after delete hook for a single document.
fn run_delete_hook(
    lua: &Lua,
    def: &CollectionDefinition,
    event: HookEvent,
    ctx: &DeleteManyCtx<'_>,
    doc_id: &str,
) -> mlua::Result<()> {
    let hook_ctx = HookContext::builder(ctx.collection, "delete")
        .data([("id".to_string(), Value::String(doc_id.to_string()))].into())
        .user(ctx.user)
        .ui_locale(ctx.ui_locale)
        .build();

    run_hooks_inner(lua, &def.hooks, event, hook_ctx)
        .map_err(|e| RuntimeError(format!("delete hook error: {e:#}")))?;

    Ok(())
}

/// Process a single document in the bulk delete loop.
fn process_single_delete(
    lua: &Lua,
    conn: &dyn DbConnection,
    def: &CollectionDefinition,
    lc: &LocaleConfig,
    ctx: &DeleteManyCtx<'_>,
    doc: &Document,
    hooks_enabled: bool,
) -> mlua::Result<bool> {
    if !ctx.soft_delete && should_skip_for_refs(conn, ctx.collection, &doc.id)? {
        return Ok(false);
    }

    if hooks_enabled {
        run_delete_hook(lua, def, HookEvent::BeforeDelete, ctx, &doc.id)?;
    }

    if !ctx.soft_delete {
        query::ref_count::before_hard_delete(conn, ctx.collection, &doc.id, &def.fields, lc)
            .map_err(|e| RuntimeError(format!("ref count error: {e:#}")))?;
    }

    if !execute_single_delete(conn, ctx, &doc.id)? {
        return Ok(false);
    }

    cleanup_after_single_delete(lua, conn, def, ctx, doc)?;

    if hooks_enabled {
        run_delete_hook(lua, def, HookEvent::AfterDelete, ctx, &doc.id)?;
    }

    Ok(true)
}

/// Delete multiple documents matching a query.
///
/// For each matched document: checks ref counts (hard delete only), runs
/// before/after delete hooks, performs the delete (soft or hard), syncs FTS,
/// cancels pending image conversions, and cleans up upload files.
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
        user: user.as_ref(),
        ui_locale: ui_locale.as_deref(),
    };

    let docs = find_docs_for_deletion(lua, conn, &def, &ctx, lc, query_table)?;
    let (hooks_enabled, _guard) = check_hook_depth(lua, run_hooks, collection, "delete_many");

    let mut deleted = 0i64;
    let mut skipped = 0i64;

    for doc in &docs {
        if process_single_delete(lua, conn, &def, lc, &ctx, doc, hooks_enabled)? {
            deleted += 1;
        } else {
            skipped += 1;
        }
    }

    let result = lua.create_table()?;
    result.set("deleted", deleted)?;
    result.set("skipped", skipped)?;

    Ok(result)
}

/// Register `crap.collections.delete_many(collection, query, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(super) fn register_delete_many(
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
