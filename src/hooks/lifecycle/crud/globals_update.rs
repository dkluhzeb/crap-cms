//! Registration of `crap.globals.update` Lua function.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table};

use crate::{
    config::LocaleConfig,
    core::SharedRegistry,
    db::LocaleContext,
    hooks::lifecycle::{
        access::{check_field_read_access_with_lua, check_field_write_access_with_lua},
        converters::*,
    },
    service::{LuaWriteHooks, WriteInput, update_global_core},
};

use super::{get_tx_conn, helpers::*};

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
    let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), lc);
    let override_access = get_opt_bool(&opts, "overrideAccess", false)?;
    let run_hooks = get_opt_bool(&opts, "hooks", true)?;
    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let def = resolve_global(reg, &slug)?;

    enforce_access(
        lua, override_access, def.access.update.as_deref(),
        None, &mut vec![], "Update access denied",
    )?;

    let mut data = lua_table_to_hashmap(&data_table)?;
    let mut join_data = lua_table_to_json_map(lua, &data_table)?;

    if !override_access {
        let denied = check_field_write_access_with_lua(lua, &def.fields, user.as_ref(), "update");
        for name in &denied {
            data.remove(name);
            join_data.remove(name);
        }
    }

    let (hooks_enabled, _guard) = check_hook_depth(lua, run_hooks, &slug, "update");

    let r = reg.read().map_err(|e| RuntimeError(format!("Registry lock: {e:#}")))?;
    let write_hooks = LuaWriteHooks {
        lua,
        user: user.as_ref(),
        ui_locale: ui_locale.as_deref(),
        override_access,
        registry: Some(&r),
        hooks_enabled,
        run_validation: run_hooks,
    };

    let write_input = WriteInput::builder(data, &join_data)
        .locale_ctx(locale_ctx.as_ref())
        .locale(locale_str)
        .ui_locale(ui_locale.clone())
        .build();

    let (mut doc, _ctx) = update_global_core(conn, &write_hooks, &slug, &def, write_input, user.as_ref())
        .map_err(|e| RuntimeError(format!("update_global error: {e:#}")))?;

    if !override_access {
        let denied = check_field_read_access_with_lua(lua, &def.fields, user.as_ref());
        for name in &denied {
            doc.fields.remove(name);
        }
    }

    document_to_lua_table(lua, &doc)
}

/// Register `crap.globals.update(slug, data, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(super) fn register_globals_update(
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
