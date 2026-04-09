//! Registration of `crap.globals.update` Lua function.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table};

use crate::{
    config::LocaleConfig,
    core::SharedRegistry,
    db::LocaleContext,
    hooks::lifecycle::{
        converters::*,
        crud::{get_tx_conn, helpers::*},
    },
    service::{LuaWriteHooks, WriteInput, update_global_core},
};

/// Core logic for `crap.globals.update`.
fn globals_update_inner(
    lua: &Lua,
    reg: &SharedRegistry,
    lc: &LocaleConfig,
    slug: String,
    data_table: Table,
    opts: Option<Table>,
) -> mlua::Result<Table> {
    // SAFETY: pointer valid for hook call duration — see TxContext pattern
    let conn_ptr = get_tx_conn(lua)?;
    let conn = unsafe { &*conn_ptr };

    let locale_str = get_opt_string(&opts, "locale")?;
    let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), lc)
        .map_err(|e| RuntimeError(e.to_string()))?;
    let override_access = get_opt_bool(&opts, "overrideAccess", false)?;
    let run_hooks = get_opt_bool(&opts, "hooks", true)?;
    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let def = resolve_global(reg, &slug)?;

    // Collection-level access check is handled inside service::update_global_core
    // via WriteHooks::check_access (respects override_access on LuaWriteHooks).

    let data = lua_table_to_hashmap(&data_table)?;
    let join_data = lua_table_to_json_map(lua, &data_table)?;

    // Field write access is now checked inside service::update_global_core
    // via WriteHooks::field_write_denied.

    let (hooks_enabled, _guard) = check_hook_depth(lua, run_hooks, &slug, "update");

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

    let write_input = WriteInput::builder(data, &join_data)
        .locale_ctx(locale_ctx.as_ref())
        .locale(locale_str)
        .ui_locale(ui_locale.clone())
        .build();

    let (doc, _ctx) =
        update_global_core(conn, &write_hooks, &slug, &def, write_input, user.as_ref())
            .map_err(|e| RuntimeError(format!("update_global error: {e:#}")))?;

    // Hydration and read-denied field stripping are handled inside
    // update_global_core via WriteHooks.

    document_to_lua_table(lua, &doc)
}

/// Register `crap.globals.update(slug, data, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_globals_update(
    lua: &Lua,
    table: &Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let lc = locale_config.clone();
    let update_fn = lua.create_function(
        move |lua, (slug, data_table, opts): (String, Table, Option<Table>)| {
            globals_update_inner(lua, &registry, &lc, slug, data_table, opts)
        },
    )?;
    table.set("update", update_fn)?;
    Ok(())
}
