//! Registration of `crap.collections.find` Lua function.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table};

use crate::{
    config::{LocaleConfig, PaginationConfig},
    core::{CollectionDefinition, Document, SharedRegistry, upload},
    db::{
        DbConnection, FindQuery, LocaleContext,
        query::{self, PaginationResult, filter::normalize_filter_fields},
    },
    hooks::lifecycle::{
        HookContext, HookEvent,
        access::check_field_read_access_with_lua,
        converters::*,
        execution::{AfterReadCtx, apply_after_read_inner, run_hooks_inner},
    },
};

use super::{get_tx_conn, helpers::*};

/// Parameters for the find operation, capturing all pre-cloned config values.
struct FindParams {
    pg_default: i64,
    pg_max: i64,
    pg_cursor: bool,
}

/// Shared context for a find operation.
struct FindCtx<'a> {
    collection: &'a str,
    depth: i32,
    override_access: bool,
    draft: bool,
    user: Option<&'a Document>,
    ui_locale: Option<&'a str>,
}

/// Convert a [`PaginationResult`] into an mlua table.
fn pagination_result_to_lua_table(lua: &Lua, pr: &PaginationResult) -> LuaResult<Table> {
    let t = lua.create_table()?;
    t.set("totalDocs", pr.total_docs)?;
    t.set("limit", pr.limit)?;
    t.set("hasNextPage", pr.has_next_page)?;
    t.set("hasPrevPage", pr.has_prev_page)?;

    if let Some(v) = pr.total_pages {
        t.set("totalPages", v)?;
    }
    if let Some(v) = pr.page {
        t.set("page", v)?;
    }
    if let Some(v) = pr.page_start {
        t.set("pageStart", v)?;
    }
    if let Some(v) = pr.prev_page {
        t.set("prevPage", v)?;
    }
    if let Some(v) = pr.next_page {
        t.set("nextPage", v)?;
    }
    if let Some(ref v) = pr.start_cursor {
        t.set("startCursor", v.clone())?;
    }
    if let Some(ref v) = pr.end_cursor {
        t.set("endCursor", v.clone())?;
    }

    Ok(t)
}

/// Build the FindQuery from the Lua table, applying pagination, filters, and access control.
fn prepare_find_query(
    lua: &Lua,
    params: &FindParams,
    def: &CollectionDefinition,
    query_table: Option<Table>,
    ctx: &FindCtx<'_>,
) -> LuaResult<(FindQuery, Option<i64>)> {
    let (mut fq, lua_page) = match query_table {
        Some(qt) => lua_table_to_find_query(&qt)?,
        None => (FindQuery::default(), None),
    };

    fq.limit = Some(query::apply_pagination_limits(
        fq.limit,
        params.pg_default,
        params.pg_max,
    ));

    if let Some(p) = lua_page {
        let clamped = fq.limit.unwrap_or(params.pg_default);
        fq.offset = Some((p.max(1) - 1) * clamped);
    }

    if !params.pg_cursor {
        fq.after_cursor = None;
        fq.before_cursor = None;
    }

    normalize_filter_fields(&mut fq.filters, &def.fields);
    add_draft_filter(def, ctx.draft, &mut fq.filters);

    enforce_access(
        lua,
        ctx.override_access,
        def.access.read.as_deref(),
        None,
        &mut fq.filters,
        "Read access denied",
    )?;

    Ok((fq, lua_page))
}

/// Fire the before_read collection hook.
fn fire_before_read(lua: &Lua, def: &CollectionDefinition, ctx: &FindCtx<'_>) -> LuaResult<()> {
    let before_ctx = HookContext::builder(ctx.collection, "find")
        .user(ctx.user)
        .ui_locale(ctx.ui_locale)
        .build();
    run_hooks_inner(lua, &def.hooks, HookEvent::BeforeRead, before_ctx)
        .map_err(|e| RuntimeError(format!("before_read hook error: {e:#}")))?;
    Ok(())
}

/// Validate, execute the find query, and count total matching documents.
fn execute_find(
    conn: &dyn DbConnection,
    def: &CollectionDefinition,
    ctx: &FindCtx<'_>,
    find_query: &FindQuery,
    locale_ctx: Option<&LocaleContext>,
) -> LuaResult<(Vec<Document>, i64)> {
    query::validate_query_fields(def, find_query, locale_ctx)
        .map_err(|e| RuntimeError(format!("find error: {e:#}")))?;

    let docs = query::find(conn, ctx.collection, def, find_query, locale_ctx)
        .map_err(|e| RuntimeError(format!("find error: {e:#}")))?;

    let total = query::count_with_search(
        conn,
        ctx.collection,
        def,
        &find_query.filters,
        locale_ctx,
        find_query.search.as_deref(),
        find_query.include_deleted,
    )
    .map_err(|e| RuntimeError(format!("count error: {e:#}")))?;

    Ok((docs, total))
}

/// Hydrate join-table data and populate relationships for all documents.
fn hydrate_and_populate(
    conn: &dyn DbConnection,
    reg: &SharedRegistry,
    def: &CollectionDefinition,
    ctx: &FindCtx<'_>,
    docs: &mut [Document],
    select: Option<&[String]>,
    locale_ctx: Option<&LocaleContext>,
) -> LuaResult<()> {
    for doc in docs.iter_mut() {
        query::hydrate_document(conn, ctx.collection, &def.fields, doc, select, locale_ctx)
            .map_err(|e| RuntimeError(format!("hydrate error: {e:#}")))?;
    }

    if ctx.depth > 0 {
        let r = reg
            .read()
            .map_err(|e| RuntimeError(format!("Registry lock: {e:#}")))?;
        let pop_ctx = query::PopulateContext::new(conn, &r, ctx.collection, def);
        let mut pop_opts = query::PopulateOpts::new(ctx.depth);
        if let Some(s) = select {
            pop_opts = pop_opts.select(s);
        }
        if let Some(lc) = locale_ctx {
            pop_opts = pop_opts.locale_ctx(lc);
        }
        query::populate_relationships_batch(&pop_ctx, docs, &pop_opts)
            .map_err(|e| RuntimeError(format!("populate error: {e:#}")))?;
    }

    Ok(())
}

/// Apply upload sizes, select stripping, and field-level read access stripping.
fn post_process_docs(
    lua: &Lua,
    def: &CollectionDefinition,
    ctx: &FindCtx<'_>,
    docs: &mut [Document],
    select: &Option<Vec<String>>,
) {
    if let Some(ref upload_config) = def.upload
        && upload_config.enabled
    {
        for doc in docs.iter_mut() {
            upload::assemble_sizes_object(doc, upload_config);
        }
    }

    if let Some(sel) = select {
        for doc in docs.iter_mut() {
            query::apply_select_to_document(doc, sel);
        }
    }

    if !ctx.override_access {
        let denied = check_field_read_access_with_lua(lua, &def.fields, ctx.user);
        if !denied.is_empty() {
            for doc in docs.iter_mut() {
                for name in &denied {
                    doc.fields.remove(name);
                }
            }
        }
    }
}

/// Build the pagination result from query results and config.
fn build_pagination_result(
    params: &FindParams,
    find_query: &FindQuery,
    docs: &[Document],
    total: i64,
    lua_page: Option<i64>,
    timestamps: bool,
) -> PaginationResult {
    let limit = find_query.limit.unwrap_or(params.pg_default);
    let had_cursor = find_query.after_cursor.is_some() || find_query.before_cursor.is_some();
    let cursor_has_more = if had_cursor && (docs.len() as i64) < limit {
        Some(false)
    } else {
        None
    };

    if params.pg_cursor {
        query::PaginationResult::builder(docs, total, limit).cursor(
            find_query.order_by.as_deref(),
            timestamps,
            find_query.before_cursor.is_some(),
            had_cursor,
            cursor_has_more,
        )
    } else {
        let offset = find_query.offset.unwrap_or(0);
        let page = lua_page
            .unwrap_or_else(|| if limit > 0 { offset / limit + 1 } else { 1 })
            .max(1);
        query::PaginationResult::builder(docs, total, limit).page(page, offset)
    }
}

/// Core logic for `crap.collections.find`.
fn find_inner(
    lua: &Lua,
    reg: &SharedRegistry,
    lc: &LocaleConfig,
    params: &FindParams,
    collection: String,
    query_table: Option<Table>,
) -> LuaResult<Table> {
    // SAFETY: pointer valid for hook call duration — see TxContext pattern
    let conn_ptr = get_tx_conn(lua)?;
    let conn = unsafe { &*conn_ptr };

    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let depth: i32 = query_table
        .as_ref()
        .and_then(|qt| qt.get::<i32>("depth").ok())
        .unwrap_or(0)
        .clamp(0, 10);
    let locale_str = get_opt_string(&query_table, "locale")?;
    let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), lc);
    let override_access = get_opt_bool(&query_table, "overrideAccess", false)?;
    let draft = get_opt_bool(&query_table, "draft", false)?;
    let def = resolve_collection(reg, &collection)?;

    let ctx = FindCtx {
        collection: &collection,
        depth,
        override_access,
        draft,
        user: user.as_ref(),
        ui_locale: ui_locale.as_deref(),
    };

    let (find_query, lua_page) = prepare_find_query(lua, params, &def, query_table, &ctx)?;

    fire_before_read(lua, &def, &ctx)?;

    let (mut docs, total) = execute_find(conn, &def, &ctx, &find_query, locale_ctx.as_ref())?;

    let select_slice = find_query.select.as_deref();
    hydrate_and_populate(
        conn,
        reg,
        &def,
        &ctx,
        &mut docs,
        select_slice,
        locale_ctx.as_ref(),
    )?;
    post_process_docs(lua, &def, &ctx, &mut docs, &find_query.select);

    // Run after_read hooks
    let ar_ctx = AfterReadCtx {
        hooks: &def.hooks,
        fields: &def.fields,
        collection: &collection,
        operation: "find",
        user: user.as_ref(),
        ui_locale: ui_locale.as_deref(),
    };
    let docs: Vec<_> = docs
        .into_iter()
        .map(|doc| apply_after_read_inner(lua, &ar_ctx, doc))
        .collect();

    let pr = build_pagination_result(params, &find_query, &docs, total, lua_page, def.timestamps);
    let pagination = pagination_result_to_lua_table(lua, &pr)?;
    find_result_to_lua(lua, &docs, pagination)
}

/// Register `crap.collections.find(collection, query?)`.
#[cfg(not(tarpaulin_include))]
pub(super) fn register_find(
    lua: &Lua,
    table: &Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
    pagination_config: &PaginationConfig,
) -> Result<()> {
    let lc = locale_config.clone();
    let params = FindParams {
        pg_default: pagination_config.default_limit,
        pg_max: pagination_config.max_limit,
        pg_cursor: pagination_config.is_cursor(),
    };

    let find_fn = lua.create_function(
        move |lua, (collection, query_table): (String, Option<Table>)| {
            find_inner(lua, &registry, &lc, &params, collection, query_table)
        },
    )?;

    table.set("find", find_fn)?;
    Ok(())
}
