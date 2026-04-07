//! Registration of `crap.globals.get` Lua function.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table};

use crate::{
    config::LocaleConfig,
    core::SharedRegistry,
    db::LocaleContext,
    hooks::lifecycle::converters::document_to_lua_table,
    service::{LuaReadHooks, get_global_document},
};

use super::{get_tx_conn, helpers::*};

/// Core logic for `crap.globals.get`.
fn globals_get_inner(
    lua: &Lua,
    reg: &SharedRegistry,
    lc: &LocaleConfig,
    slug: String,
    opts: Option<Table>,
) -> mlua::Result<Table> {
    // SAFETY: pointer valid for hook call duration — see TxContext pattern
    let conn_ptr = get_tx_conn(lua)?;
    let conn = unsafe { &*conn_ptr };

    let locale_str = get_opt_string(&opts, "locale")?;
    let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), lc);
    let override_access = get_opt_bool(&opts, "overrideAccess", false)?;
    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let def = resolve_global(reg, &slug)?;

    enforce_access(
        lua,
        override_access,
        def.access.read.as_deref(),
        None,
        &mut vec![],
        "Read access denied",
    )?;

    let hooks = LuaReadHooks {
        lua,
        user: user.as_ref(),
        ui_locale: ui_locale.as_deref(),
        override_access,
    };

    let doc = get_global_document(
        conn,
        &hooks,
        &slug,
        &def,
        locale_ctx.as_ref(),
        user.as_ref(),
        ui_locale.as_deref(),
    )
    .map_err(|e| RuntimeError(format!("get_global error: {e:#}")))?;

    document_to_lua_table(lua, &doc)
}

/// Register `crap.globals.get(slug, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(super) fn register_globals_get(
    lua: &Lua,
    table: &Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let lc = locale_config.clone();
    let get_fn = lua.create_function(move |lua, (slug, opts): (String, Option<Table>)| {
        globals_get_inner(lua, &registry, &lc, slug, opts)
    })?;
    table.set("get", get_fn)?;
    Ok(())
}
