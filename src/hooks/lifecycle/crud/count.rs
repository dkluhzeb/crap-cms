//! Registration of `crap.collections.count` Lua function.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table};

use crate::{
    config::LocaleConfig,
    core::SharedRegistry,
    db::{
        LocaleContext,
        query::{self, filter::normalize_filter_fields},
    },
    hooks::lifecycle::converters::lua_table_to_find_query,
};

use super::{get_tx_conn, helpers::*};

/// Core logic for `crap.collections.count`.
fn count_inner(
    lua: &Lua,
    reg: &SharedRegistry,
    lc: &LocaleConfig,
    collection: String,
    query_table: Option<Table>,
) -> LuaResult<i64> {
    // SAFETY: pointer valid for hook call duration — see TxContext pattern
    let conn_ptr = get_tx_conn(lua)?;
    let conn = unsafe { &*conn_ptr };

    let locale_ctx =
        LocaleContext::from_locale_string(get_opt_string(&query_table, "locale")?.as_deref(), lc);
    let override_access = get_opt_bool(&query_table, "overrideAccess", false)?;
    let draft = get_opt_bool(&query_table, "draft", false)?;
    let def = resolve_collection(reg, &collection)?;

    let find_query = match query_table {
        Some(ref qt) => lua_table_to_find_query(qt)?.0,
        None => query::FindQuery::default(),
    };
    let mut filters = find_query.filters;
    let search = find_query.search;

    normalize_filter_fields(&mut filters, &def.fields);
    add_draft_filter(&def, draft, &mut filters);
    enforce_access(
        lua,
        override_access,
        def.access.read.as_deref(),
        None,
        &mut filters,
        "Read access denied",
    )?;

    query::count_with_search(
        conn,
        &collection,
        &def,
        &filters,
        locale_ctx.as_ref(),
        search.as_deref(),
        false,
    )
    .map_err(|e| RuntimeError(format!("count error: {e:#}")))
}

/// Register `crap.collections.count(collection, query?)`.
#[cfg(not(tarpaulin_include))]
pub(super) fn register_count(
    lua: &Lua,
    table: &Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let lc = locale_config.clone();
    let count_fn = lua.create_function(
        move |lua, (collection, query_table): (String, Option<Table>)| {
            count_inner(lua, &registry, &lc, collection, query_table)
        },
    )?;

    table.set("count", count_fn)?;

    Ok(())
}
