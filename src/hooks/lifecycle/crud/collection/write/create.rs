//! Registration of `crap.collections.create` Lua function.

use std::collections::HashMap;

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table};
use serde_json::Value;

use crate::{
    config::LocaleConfig,
    core::SharedRegistry,
    db::LocaleContext,
    hooks::lifecycle::{
        converters::*,
        crud::{get_tx_conn, helpers::*},
    },
    service::{LuaWriteHooks, WriteInput, create_document_core},
};

/// Execute the `crap.collections.create` operation.
fn create_document(
    lua: &Lua,
    reg: &SharedRegistry,
    lc: &LocaleConfig,
    collection: String,
    data_table: Table,
    opts: Option<Table>,
) -> mlua::Result<Table> {
    // SAFETY: pointer valid for hook call duration — see TxContext pattern
    let conn_ptr = get_tx_conn(lua)?;
    let conn = unsafe { &*conn_ptr };

    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let locale_str = get_opt_string(&opts, "locale")?;
    let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), lc)
        .map_err(|e| RuntimeError(e.to_string()))?;
    let override_access = get_opt_bool(&opts, "overrideAccess", false)?;
    let run_hooks = get_opt_bool(&opts, "hooks", true)?;
    let draft = get_opt_bool(&opts, "draft", false)?;
    let def = resolve_collection(reg, &collection)?;

    // Collection-level access check is handled inside service::create_document_core
    // via WriteHooks::check_access (respects override_access on LuaWriteHooks).

    let ExtractedData {
        flat,
        hook,
        password,
    } = extract_data(lua, &data_table, &def)?;

    // Field write access is now checked inside service::create_document_core
    // via WriteHooks::field_write_denied.

    let (hooks_enabled, _guard) = check_hook_depth(lua, run_hooks, &collection, "create");

    // Separate join data (non-string values) from the merged hook map
    let join_data: HashMap<String, Value> = hook
        .iter()
        .filter(|(_, v)| !matches!(v, Value::String(_)))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

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

    let write_input = WriteInput::builder(flat, &join_data)
        .password(password.as_deref())
        .locale_ctx(locale_ctx.as_ref())
        .locale(locale_str)
        .draft(draft)
        .ui_locale(ui_locale.clone())
        .build();

    let (doc, _ctx) = create_document_core(
        conn,
        &write_hooks,
        &collection,
        &def,
        write_input,
        user.as_ref(),
    )
    .map_err(|e| RuntimeError(format!("create error: {e:#}")))?;

    // Hydration and read-denied field stripping are handled inside
    // create_document_core via WriteHooks.

    document_to_lua_table(lua, &doc)
}

/// Register `crap.collections.create(collection, data, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_create(
    lua: &Lua,
    table: &Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let lc = locale_config.clone();
    let create_fn = lua.create_function(
        move |lua, (collection, data_table, opts): (String, mlua::Table, Option<mlua::Table>)| {
            create_document(lua, &registry, &lc, collection, data_table, opts)
        },
    )?;

    table.set("create", create_fn)?;

    Ok(())
}
