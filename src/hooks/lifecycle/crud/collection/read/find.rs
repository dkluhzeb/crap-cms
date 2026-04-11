//! Registration of `crap.collections.find` Lua function.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table};

use crate::{
    config::{LocaleConfig, PaginationConfig},
    core::{CollectionDefinition, SharedRegistry},
    db::{
        FindQuery, LocaleContext,
        query::{self, filter::normalize_filter_fields},
    },
    hooks::lifecycle::{
        converters::*,
        crud::{get_tx_conn, helpers::*},
    },
    service::{FindDocumentsInput, LuaReadHooks, ServiceContext, find_documents},
};

/// Parameters for the find operation, capturing all pre-cloned config values.
struct FindParams {
    pg_default: i64,
    pg_max: i64,
    pg_cursor: bool,
}

/// Query-building context for the find operation.
struct FindCtx {
    draft: bool,
}

// Note: `override_access` is parsed from opts and passed to `LuaReadHooks`
// (which passes it to the service layer's `check_access`), not to `FindCtx`.

/// Build the FindQuery from the Lua table, applying pagination, filters, and access control.
fn prepare_find_query(
    params: &FindParams,
    def: &CollectionDefinition,
    query_table: Option<Table>,
    ctx: &FindCtx,
) -> LuaResult<FindQuery> {
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

    Ok(fq)
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
    let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), lc)
        .map_err(|e| RuntimeError(e.to_string()))?;
    let override_access = get_opt_bool(&query_table, "overrideAccess", false)?;
    let draft = get_opt_bool(&query_table, "draft", false)?;
    let def = resolve_collection(reg, &collection)?;

    let ctx = FindCtx { draft };

    let find_query = prepare_find_query(params, &def, query_table, &ctx)?;

    let r = reg
        .read()
        .map_err(|e| RuntimeError(format!("Registry lock: {e:#}")))?;

    let hooks = LuaReadHooks::builder(lua)
        .user(user.as_ref())
        .ui_locale(ui_locale.as_deref())
        .override_access(override_access)
        .build();

    let ctx = ServiceContext::collection(&collection, &def)
        .conn(conn)
        .read_hooks(&hooks)
        .user(user.as_ref())
        .override_access(override_access)
        .build();

    let input = FindDocumentsInput::builder(&find_query)
        .depth(depth)
        .locale_ctx(locale_ctx.as_ref())
        .registry(Some(&r))
        .select(find_query.select.as_deref())
        .cursor_enabled(params.pg_cursor)
        .build();

    let result = find_documents(&ctx, &input).map_err(|e| RuntimeError(format!("{e}")))?;

    let pagination = pagination_result_to_lua_table(lua, &result.pagination)?;
    find_result_to_lua(lua, &result.docs, pagination)
}

/// Register `crap.collections.find(collection, query?)`.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_find(
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
