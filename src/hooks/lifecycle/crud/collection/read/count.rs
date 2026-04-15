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
    hooks::lifecycle::{
        converters::lua_table_to_find_query,
        crud::{get_tx_conn, helpers::*},
    },
    service::{CountDocumentsInput, LuaReadHooks, ServiceContext, count_documents},
};

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
        LocaleContext::from_locale_string(get_opt_string(&query_table, "locale")?.as_deref(), lc)
            .map_err(|e| RuntimeError(e.to_string()))?;
    let override_access = get_opt_bool(&query_table, "overrideAccess", false)?;
    let draft = get_opt_bool(&query_table, "draft", false)?;
    let user = hook_user(lua);
    let def = resolve_collection(reg, &collection)?;

    let find_query = match query_table {
        Some(ref qt) => lua_table_to_find_query(qt)?.0,
        None => query::FindQuery::default(),
    };
    let mut filters = find_query.filters;
    let search = find_query.search;

    normalize_filter_fields(&mut filters, &def.fields);

    let hooks = LuaReadHooks::builder(lua)
        .user(user.as_ref())
        .override_access(override_access)
        .build();

    let ctx = ServiceContext::collection(&collection, &def)
        .conn(conn)
        .read_hooks(&hooks)
        .user(user.as_ref())
        .override_access(override_access)
        .build();

    let input = CountDocumentsInput::builder(&filters)
        .locale_ctx(locale_ctx.as_ref())
        .search(search.as_deref())
        .include_drafts(draft)
        .build();

    count_documents(&ctx, &input).map_err(|e| RuntimeError(format!("{e}")))
}

/// Register `crap.collections.count(collection, query?)`.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_count(
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
