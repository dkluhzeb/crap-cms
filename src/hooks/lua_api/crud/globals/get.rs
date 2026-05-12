//! Registration of `crap.globals.get` Lua function.

use std::sync::Arc;

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table};

use crate::{
    config::LocaleConfig,
    core::Registry,
    db::LocaleContext,
    hooks::{
        lifecycle::converters::document_to_lua_table,
        lua_api::crud::{get_tx_conn, helpers::*},
    },
    service::{GetGlobalInput, LuaReadHooks, ServiceContext, get_global_document},
};

/// Core logic for `crap.globals.get`.
fn globals_get_inner(
    lua: &Lua,
    reg: &Registry,
    lc: &LocaleConfig,
    slug: String,
    opts: Option<Table>,
) -> mlua::Result<Table> {
    let conn = get_tx_conn(lua)?;

    let locale_str = get_opt_string(&opts, "locale")?;
    let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), lc)
        .map_err(|e| RuntimeError(e.to_string()))?;
    let override_access = get_opt_bool(&opts, "overrideAccess", false)?;
    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let def = resolve_global(reg, &slug)?;

    let hooks = LuaReadHooks::builder(lua)
        .user(user.as_ref())
        .ui_locale(ui_locale.as_deref())
        .override_access(override_access)
        .build();

    let ctx = ServiceContext::global(&slug, &def)
        .conn(conn)
        .read_hooks(&hooks)
        .user(user.as_ref())
        .override_access(override_access)
        .build();

    let input = GetGlobalInput::new(locale_ctx.as_ref(), ui_locale.as_deref());

    let doc = get_global_document(&ctx, &input).map_err(|e| RuntimeError(format!("{e}")))?;

    document_to_lua_table(lua, &doc)
}

/// Register `crap.globals.get(slug, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_globals_get(
    lua: &Lua,
    table: &Table,
    registry: Arc<Registry>,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let lc = locale_config.clone();
    let get_fn = lua.create_function(move |lua, (slug, opts): (String, Option<Table>)| {
        globals_get_inner(lua, &registry, &lc, slug, opts)
    })?;
    table.set("get", get_fn)?;
    Ok(())
}
