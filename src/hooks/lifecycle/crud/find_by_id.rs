//! Registration of `crap.collections.find_by_id` Lua function.

use std::collections::HashSet;

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table, Value};

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, Document, SharedRegistry, upload},
    db::{DbConnection, FilterClause, LocaleContext, ops, query},
    hooks::lifecycle::{
        HookContext, HookEvent,
        access::check_field_read_access_with_lua,
        converters::document_to_lua_table,
        execution::{AfterReadCtx, apply_after_read_inner, run_hooks_inner},
    },
};

use super::{get_tx_conn, helpers::*};

/// Context for a find_by_id operation.
struct FindByIdCtx<'a> {
    collection: &'a str,
    id: &'a str,
    depth: i32,
    override_access: bool,
    user: Option<&'a Document>,
    ui_locale: Option<&'a str>,
}

/// Fire the before_read collection hook.
fn fire_before_read(lua: &Lua, def: &CollectionDefinition, ctx: &FindByIdCtx<'_>) -> LuaResult<()> {
    let before_ctx = HookContext::builder(ctx.collection, "find_by_id")
        .user(ctx.user)
        .ui_locale(ctx.ui_locale)
        .build();

    run_hooks_inner(lua, &def.hooks, HookEvent::BeforeRead, before_ctx)
        .map_err(|e| RuntimeError(format!("before_read hook error: {e:#}")))?;

    Ok(())
}

/// Check access control and return optional constraint filters.
fn resolve_access_constraints(
    lua: &Lua,
    def: &CollectionDefinition,
    ctx: &FindByIdCtx<'_>,
) -> LuaResult<Option<Vec<FilterClause>>> {
    let mut filters: Vec<FilterClause> = Vec::new();

    enforce_access(
        lua,
        ctx.override_access,
        def.access.read.as_deref(),
        Some(ctx.id),
        &mut filters,
        "Read access denied",
    )?;

    Ok(if filters.is_empty() {
        None
    } else {
        Some(filters)
    })
}

/// Populate relationships for a single document.
fn populate_doc(
    conn: &dyn DbConnection,
    reg: &SharedRegistry,
    def: &CollectionDefinition,
    ctx: &FindByIdCtx<'_>,
    doc: &mut Document,
    select: Option<&[String]>,
    locale_ctx: Option<&LocaleContext>,
) -> LuaResult<()> {
    if ctx.depth <= 0 {
        return Ok(());
    }

    let r = reg
        .read()
        .map_err(|e| RuntimeError(format!("Registry lock: {e:#}")))?;
    let mut visited = HashSet::new();
    let pop_ctx = query::PopulateContext::new(conn, &r, ctx.collection, def);
    let mut pop_opts = query::PopulateOpts::new(ctx.depth);

    if let Some(s) = select {
        pop_opts = pop_opts.select(s);
    }

    if let Some(lc) = locale_ctx {
        pop_opts = pop_opts.locale_ctx(lc);
    }

    query::populate_relationships(&pop_ctx, doc, &mut visited, &pop_opts)
        .map_err(|e| RuntimeError(format!("populate error: {e:#}")))?;

    Ok(())
}

/// Apply upload sizes, select stripping, and field-level read access stripping.
fn apply_post_filters(
    lua: &Lua,
    def: &CollectionDefinition,
    ctx: &FindByIdCtx<'_>,
    doc: &mut Document,
    select: &Option<Vec<String>>,
) {
    if let Some(ref upload_config) = def.upload
        && upload_config.enabled
    {
        upload::assemble_sizes_object(doc, upload_config);
    }

    if let Some(sel) = select {
        query::apply_select_to_document(doc, sel);
    }

    if !ctx.override_access {
        let denied = check_field_read_access_with_lua(lua, &def.fields, ctx.user);

        for name in &denied {
            doc.fields.remove(name);
        }
    }
}

/// Core logic for `crap.collections.find_by_id`.
fn find_by_id_inner(
    lua: &Lua,
    reg: &SharedRegistry,
    lc: &LocaleConfig,
    collection: String,
    id: String,
    opts: Option<Table>,
) -> LuaResult<Value> {
    // SAFETY: pointer valid for hook call duration — see TxContext pattern
    let conn_ptr = get_tx_conn(lua)?;
    let conn = unsafe { &*conn_ptr };

    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let depth: i32 = opts
        .as_ref()
        .and_then(|o| o.get::<i32>("depth").ok())
        .unwrap_or(0)
        .clamp(0, 10);
    let locale_str = get_opt_string(&opts, "locale")?;
    let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), lc);
    let override_access = get_opt_bool(&opts, "overrideAccess", false)?;
    let use_draft = get_opt_bool(&opts, "draft", false)?;
    let def = resolve_collection(reg, &collection)?;

    let select: Option<Vec<String>> = opts
        .as_ref()
        .and_then(|o| o.get::<Table>("select").ok())
        .map(|t| {
            t.sequence_values::<String>()
                .filter_map(|r| r.ok())
                .collect()
        });

    let ctx = FindByIdCtx {
        collection: &collection,
        id: &id,
        depth,
        override_access,
        user: user.as_ref(),
        ui_locale: ui_locale.as_deref(),
    };

    fire_before_read(lua, &def, &ctx)?;

    let access_constraints = resolve_access_constraints(lua, &def, &ctx)?;

    let mut doc = ops::find_by_id_full(
        conn,
        ctx.collection,
        &def,
        ctx.id,
        locale_ctx.as_ref(),
        access_constraints,
        use_draft,
    )
    .map_err(|e| RuntimeError(format!("find_by_id error: {e:#}")))?;

    if let Some(ref mut d) = doc {
        let select_slice = select.as_deref();

        populate_doc(conn, reg, &def, &ctx, d, select_slice, locale_ctx.as_ref())?;

        apply_post_filters(lua, &def, &ctx, d, &select);
    }

    // Run after_read hooks
    let ar_ctx = AfterReadCtx {
        hooks: &def.hooks,
        fields: &def.fields,
        collection: &collection,
        operation: "find_by_id",
        user: ctx.user,
        ui_locale: ctx.ui_locale,
    };
    let doc = doc.map(|d| apply_after_read_inner(lua, &ar_ctx, d));

    match doc {
        Some(d) => Ok(Value::Table(document_to_lua_table(lua, &d)?)),
        None => Ok(Value::Nil),
    }
}

/// Register `crap.collections.find_by_id(collection, id, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(super) fn register_find_by_id(
    lua: &Lua,
    table: &Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let lc = locale_config.clone();
    let find_by_id_fn = lua.create_function(
        move |lua, (collection, id, opts): (String, String, Option<Table>)| {
            find_by_id_inner(lua, &registry, &lc, collection, id, opts)
        },
    )?;

    table.set("find_by_id", find_by_id_fn)?;

    Ok(())
}
