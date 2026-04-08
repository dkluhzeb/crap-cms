//! Registration of `crap.collections.find_by_id` Lua function.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table, Value};

use crate::{
    config::LocaleConfig,
    core::SharedRegistry,
    db::LocaleContext,
    hooks::lifecycle::converters::document_to_lua_table,
    service::{LuaReadHooks, ReadOptions, find_document_by_id},
};

use super::{get_tx_conn, helpers::*};

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

    let r = reg
        .read()
        .map_err(|e| RuntimeError(format!("Registry lock: {e:#}")))?;
    let hooks = LuaReadHooks {
        lua,
        user: user.as_ref(),
        ui_locale: ui_locale.as_deref(),
        override_access,
    };
    let opts = ReadOptions {
        depth,
        locale_ctx: locale_ctx.as_ref(),
        registry: Some(&r),
        select: select.as_deref(),
        user: user.as_ref(),
        ui_locale: ui_locale.as_deref(),
        use_draft,
        ..Default::default()
    };

    let doc = find_document_by_id(conn, &hooks, &collection, &def, &id, &opts)
        .map_err(|e| RuntimeError(format!("{e}")))?;

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
